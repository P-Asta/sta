//! Reducer scenarios: archive (manual, auto, retention, restore), Clear Today, the Ctrl+Tab
//! switcher, downloads, permission prompts, find, zoom, fullscreen, the command bar and toasts.

mod common;
use sta_core::*;
use common::*;

fn download(id: u32, tab: Option<Id>, state: DownloadState, received: i64) -> Download {
    Download {
        id,
        tab,
        url: format!("https://files.example.com/f{id}.zip"),
        file_name: format!("f{id}.zip"),
        path: Some(format!("C:\\Users\\me\\Downloads\\f{id}.zip")),
        received_bytes: received,
        total_bytes: Some(1000),
        bytes_per_sec: 100,
        state,
        started_at: 0,
    }
}

// ------------------------------------------------------------------------------------ archive

#[test]
fn auto_archive_after_idle_timeout() {
    let mut h = Harness::new();
    let a = h.open("https://a.com/");
    let b = h.open("https://b.com/");
    let c = h.open("https://c.com/");
    let audible = h.open("https://music.com/");
    h.apply(Command::TabAudioChanged { tab: audible, audible: true });
    let p = h.open_pinned("https://pinned.com/");
    let f = h.open_favorite("https://fav.com/");
    h.apply(Command::ActivateItem { id: a });
    h.advance(11 * HOUR);
    assert!(h.apply(Command::Tick).is_empty());
    assert_eq!(h.today().len(), 4);
    // Viewing a tab resets its timer.
    h.apply(Command::ActivateItem { id: c });
    h.apply(Command::ActivateItem { id: a });
    h.advance(HOUR);
    let fx = h.apply(Command::Tick);
    assert!(has(&fx, |e| is_destroy(e, b)), "{fx:?}");
    assert_eq!(h.today(), vec![audible, c, a]);
    let entry = h.store.state().archive[0].clone();
    assert_eq!((entry.id, entry.reason), (b, ArchiveReason::Auto));
    assert!(h.store.state().reopen.is_empty(), "auto-archive never pushes onto the reopen stack");
    assert_eq!(h.ui().archive_count, 1);
    // The visible tab is refreshed by every Tick and never archived.
    h.advance(11 * HOUR + 59 * MIN);
    h.apply(Command::Tick);
    assert_eq!(h.today(), vec![audible, a], "c expired");
    h.advance(24 * HOUR);
    h.apply(Command::Tick);
    assert_eq!(h.today(), vec![audible, a]);
    assert!(h.pinned().contains(&p) && h.favorites().contains(&f), "pinned/favorites never auto-archive");
    // Audio stopped → archived at the next tick.
    h.apply(Command::TabAudioChanged { tab: audible, audible: false });
    h.apply(Command::Tick);
    assert_eq!(h.today(), vec![a]);
    // Longer setting (snapped to a supported value).
    h.apply(Command::UpdateSettings { patch: SettingsPatch { archive_after_hours: Some(30), ..Default::default() } });
    assert_eq!(h.store.settings().archive_after_hours, 24);
    let d = h.open("https://d.com/");
    h.apply(Command::ActivateItem { id: a });
    h.advance(23 * HOUR);
    h.apply(Command::Tick);
    assert!(h.today().contains(&d));
    h.advance(HOUR);
    h.apply(Command::Tick);
    assert!(!h.today().contains(&d));
    h.apply(Command::UpdateSettings { patch: SettingsPatch { archive_after_hours: Some(1000), ..Default::default() } });
    assert_eq!(h.store.settings().archive_after_hours, 720);
}

#[test]
fn auto_archive_splits_as_a_group_and_restore_whole_group() {
    let mut h = Harness::new();
    let x = h.open("https://x.com/");
    let a = h.open("https://a.com/");
    let b = h.open("https://b.com/");
    h.apply(Command::SplitWith { tab: b, with: a, side: SplitSide::Right });
    let sid = h.active().unwrap();
    h.apply(Command::ActivateItem { id: x });
    h.advance(12 * HOUR);
    h.apply(Command::Tick);
    assert_eq!(h.today(), vec![x]);
    let entries: Vec<ArchiveEntry> = h.store.state().archive.clone();
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().all(|e| e.split.as_ref().map(|s| s.group) == Some(sid) && e.reason == ArchiveReason::Auto));
    // Restore one pane alone (group split is gone): a normal Today tab.
    h.apply(Command::RestoreArchived { id: b, whole_group: false });
    assert_eq!(h.active(), Some(b));
    assert_eq!(h.store.tab_section(b), Some(Section::Today));
    // The other entry still offers its group. `b` sits where the split was, so restoring `a`
    // rejoins it: the split is rebuilt (same id, order and orientation) with `a` focused.
    h.apply(Command::RestoreArchived { id: a, whole_group: true });
    assert_eq!(h.active(), Some(sid));
    assert_eq!(h.split(sid).panes, vec![a, b]);
    assert_eq!(h.focused(), Some(a));
    assert_eq!(h.today(), vec![sid, x]);
    assert!(h.store.state().archive.is_empty());
    assert!(h.apply(Command::RestoreArchived { id: a, whole_group: false }).is_empty());
}

#[test]
fn archive_retention_delete_clear_and_list() {
    let mut h = Harness::new();
    let home_icon = h.space_data(h.space()).icon;
    let a = h.open("https://a.com/?utm_source=x&id=1");
    h.apply(Command::TabTitleChanged { tab: a, title: "A page".into() });
    h.apply(Command::RenameItem { id: a, title: Some("Custom A".into()) });
    let b = h.open("https://b.com/");
    let c = h.open("https://c.com/");
    h.apply(Command::CloseItem { id: Some(a) });
    h.advance(10 * DAY);
    h.apply(Command::CloseItem { id: Some(b) });
    let rev = h.ui().archive_revision;
    let list = h.store.archive_list();
    assert_eq!(list.iter().map(|e| e.id).collect::<Vec<_>>(), [b, a], "newest first");
    assert_eq!((list[1].title.as_str(), list[1].host.as_str()), ("Custom A", "a.com"));
    assert_eq!(list[1].space_icon.as_deref(), Some(home_icon.as_str()));
    assert_eq!(list[1].space_name.as_deref(), Some("Home"));
    // Copy URL of an archive entry (cleaned).
    let fx = h.apply(Command::CopyUrl { id: Some(a), markdown: false });
    assert!(has(&fx, |e| matches!(e, Effect::CopyToClipboard { text } if text == "https://a.com/?id=1")));
    // Retention: 30 days.
    h.advance(21 * DAY);
    h.apply(Command::Tick);
    assert_eq!(h.store.archive_list().iter().map(|e| e.id).collect::<Vec<_>>(), [b]);
    assert!(h.ui().archive_revision > rev);
    h.apply(Command::DeleteArchived { id: b });
    assert!(h.store.state().archive.is_empty());
    assert!(h.apply(Command::DeleteArchived { id: b }).is_empty());
    h.apply(Command::CloseItem { id: Some(c) });
    let rev = h.ui().archive_revision;
    h.apply(Command::ClearArchive);
    assert!(h.store.state().archive.is_empty());
    assert!(h.ui().archive_revision > rev);
    assert!(!h.store.state().reopen.is_empty());
    assert!(!h.ui().can_reopen, "entries whose archive entries are gone can't be reopened");
    assert!(h.apply(Command::ReopenClosed).len() <= 1, "stale entries are popped silently");
    assert!(h.store.state().reopen.is_empty());
}

#[test]
fn startup_auto_archives_tabs_that_expired_while_closed() {
    let mut h = Harness::new();
    let a = h.open("https://a.com/");
    let b = h.open("https://b.com/");
    let (store, _) = Store::load(Some(&h.store.state_json()), None, h.now + 2 * DAY);
    let h2 = Harness::start_at(store, Vec::new(), h.now + 2 * DAY);
    assert_eq!(h2.today(), vec![b], "active item survives, the idle one is archived");
    assert_eq!(h2.store.state().archive[0].id, a);
}

// ------------------------------------------------------------------------------------ clear today

#[test]
fn clear_today_and_undo() {
    let mut h = Harness::new();
    let p = h.open_pinned("https://p.com/");
    let a = h.open("https://a.com/");
    let b = h.open("https://b.com/");
    let music = h.open("https://music.com/");
    h.apply(Command::TabAudioChanged { tab: music, audible: true });
    let c = h.open("https://c.com/");
    let d = h.open("https://d.com/");
    h.apply(Command::SplitWith { tab: d, with: c, side: SplitSide::Right });
    let visible_split = h.active().unwrap();
    let e = h.open("https://e.com/");
    let f = h.open("https://f.com/");
    h.apply(Command::SplitWith { tab: f, with: e, side: SplitSide::Right });
    let hidden_split = h.active().unwrap();
    h.apply(Command::ActivateItem { id: visible_split });
    // today: [hidden_split, visible_split, music, b, a]
    assert_eq!(h.today(), vec![hidden_split, visible_split, music, b, a]);
    let fx = h.apply(Command::ClearToday { space: None });
    assert!(has(&fx, |e| is_destroy(e, a)) && has(&fx, |x| is_destroy(x, e)));
    assert_eq!(h.today(), vec![visible_split, music]);
    assert_eq!(h.pinned(), vec![p]);
    let toast = h.toast().unwrap();
    assert_eq!((toast.message.as_str(), toast.duration_ms), ("Cleared 4 tabs", 6000));
    let action = toast.action.unwrap();
    assert_eq!((action.label.as_str(), &*action.command), ("Undo", &Command::ReopenClosed));
    assert!(h.store.state().archive.iter().all(|e| e.reason == ArchiveReason::ClearToday));
    assert!(matches!(h.store.state().reopen.last(), Some(ReopenEntry::Batch { archive_ids }) if archive_ids.len() == 4));
    // Undo (the toast's command): original order, nothing activated.
    let fx = h.apply(*action.command);
    assert_eq!(h.today(), vec![hidden_split, visible_split, music, b, a]);
    assert_eq!(h.split(hidden_split).panes, vec![e, f]);
    assert_eq!(h.active(), Some(visible_split));
    assert!(!has(&fx, |e| matches!(e, Effect::CreateBrowser { .. })), "restored tabs stay unloaded");
    assert!(!h.store.is_loaded(a));
    // Nothing to clear: no toast, no reopen entry.
    let mut h = Harness::new();
    h.open("https://only.com/");
    let reopen = h.store.state().reopen.len();
    h.apply(Command::ClearToday { space: None });
    assert!(h.toast().is_none());
    assert_eq!(h.store.state().reopen.len(), reopen);
    // Another space: everything goes (nothing visible there).
    let home = h.space();
    let work = h.new_space("Work", "🚀", Theme::default());
    h.open("https://w.com/");
    h.apply(Command::SwitchSpace { id: home });
    h.apply(Command::ClearToday { space: Some(work) });
    assert!(h.space_data(work).today.is_empty());
    assert_eq!(h.space_data(work).active_item, None);
    assert_eq!(h.toast().unwrap().message, "Cleared 1 tab");
}

// ------------------------------------------------------------------------------------ switcher

#[test]
fn mru_switcher_step_commit_cancel() {
    let mut h = Harness::new();
    let a = h.open("https://a.com/");
    let b = h.open("https://b.com/");
    let c = h.open("https://c.com/");
    let d = h.open("https://d.com/");
    assert_eq!(h.store.state().window.mru, vec![d, c, b, a]);
    let fx = h.apply(Command::MruStep { forward: true });
    assert_eq!(fx, vec![Effect::ShowSwitcher]);
    let sw = h.ui().switcher.unwrap();
    assert_eq!((sw.tabs.iter().map(|t| t.id).collect::<Vec<_>>(), sw.selected), (vec![d, c, b, a], 1));
    assert_eq!(h.apply(Command::MruStep { forward: true }), vec![]);
    assert_eq!(h.ui().switcher.unwrap().selected, 2);
    assert_eq!(h.store.state().window.mru, vec![d, c, b, a], "MRU changes only on commit");
    let fx = h.apply(Command::MruCommit);
    assert!(has(&fx, |e| matches!(e, Effect::HideSwitcher)));
    assert!(has(&fx, |e| matches!(e, Effect::FocusBrowser { tab } if *tab == b)));
    assert_eq!(h.active(), Some(b));
    assert_eq!(h.store.state().window.mru, vec![b, d, c, a]);
    assert!(h.ui().switcher.is_none());
    // Backwards wraps to the oldest card; steps wrap within the cards.
    h.apply(Command::MruStep { forward: false });
    assert_eq!(h.ui().switcher.unwrap().selected, 3);
    h.apply(Command::MruStep { forward: true });
    assert_eq!(h.ui().switcher.unwrap().selected, 0);
    assert_eq!(h.apply(Command::MruCancel), vec![Effect::HideSwitcher]);
    assert_eq!(h.active(), Some(b));
    assert!(h.apply(Command::MruCommit).is_empty());
    // Click on a card.
    h.apply(Command::MruStep { forward: true });
    h.apply(Command::MruSelect { index: 3 });
    assert_eq!(h.active(), Some(a));
    // Window deactivation cancels.
    h.apply(Command::MruStep { forward: true });
    let fx = h.apply(Command::WindowStateChanged { maximized: false, fullscreen: false, focused: false, bounds: None });
    assert!(has(&fx, |e| matches!(e, Effect::HideSwitcher)));
    assert!(!h.ui().window.focused);
    // Five cards max, across spaces.
    let e = h.open("https://e.com/");
    let work = h.new_space("Work", "🚀", Theme::default());
    let w = h.open("https://w.com/");
    h.apply(Command::MruStep { forward: true });
    let sw = h.ui().switcher.unwrap();
    assert_eq!(sw.tabs.len(), 5);
    assert_eq!(sw.tabs[1].id, e);
    assert_eq!(sw.tabs[0].space, Some(work));
    h.apply(Command::MruCommit);
    assert_ne!(h.space(), work, "committing a tab of another space switches space");
    assert_eq!(h.active(), Some(e));
    let _ = w;
    // Fewer than two tabs: no switcher.
    let mut h = Harness::new();
    h.open("https://solo.com/");
    assert!(h.apply(Command::MruStep { forward: true }).is_empty());
}

// ------------------------------------------------------------------------------------ downloads

#[test]
fn downloads_defer_browser_destruction() {
    let mut h = Harness::new();
    let keep = h.open("https://keep.com/");
    let a = h.open("https://a.com/");
    h.apply(Command::DownloadUpdated { download: download(1, Some(a), DownloadState::InProgress, 10) });
    let ui = h.ui();
    assert_eq!(ui.downloads.len(), 1);
    assert_eq!(ui.downloads[0].started_at, h.now, "missing start time is filled in");
    // Closing the tab keeps its browser alive while the download runs.
    let fx = h.apply(Command::CloseItem { id: Some(a) });
    assert!(!has(&fx, |e| is_destroy(e, a)), "{fx:?}");
    assert_eq!(h.active(), Some(keep));
    assert!(h.live.contains(&a));
    // Stale page events from the kept browser are ignored.
    h.apply(Command::TabTitleChanged { tab: a, title: "late".into() });
    h.apply(Command::DownloadUpdated { download: download(1, Some(a), DownloadState::InProgress, 500) });
    assert_eq!(h.ui().downloads[0].received_bytes, 500);
    // Completion destroys it and toasts.
    let fx = h.apply(Command::DownloadUpdated { download: download(1, Some(a), DownloadState::Complete, 1000) });
    assert!(has(&fx, |e| is_destroy(e, a)));
    assert!(!h.live.contains(&a));
    let toast = h.toast().unwrap();
    assert_eq!(toast.message, "Downloaded f1.zip");
    assert_eq!(*toast.action.unwrap().command, Command::DownloadControl { id: 1, action: DownloadAction::Open });
    // Identical update: nothing.
    let rev = h.store.revision();
    assert!(h.apply(Command::DownloadUpdated { download: download(1, Some(a), DownloadState::Complete, 1000) }).is_empty());
    assert_eq!(h.store.revision(), rev);

    // Reopening a tab whose browser is still kept for a download revives the same browser.
    let b = h.open("https://b.com/");
    h.apply(Command::DownloadUpdated { download: download(2, Some(b), DownloadState::Paused, 10) });
    h.apply(Command::CloseItem { id: Some(b) });
    assert!(h.live.contains(&b));
    let fx = h.apply(Command::ReopenClosed);
    assert!(!has(&fx, |e| is_create(e, b)), "no second CreateBrowser: {fx:?}");
    assert_eq!(shows(&fx), Some(ContentLayout::Single { tab: b }));
    assert!(h.store.is_loaded(b));
    // Unloading a pinned tab with a download defers too; cancel destroys.
    let p = h.open_pinned("https://p.com/");
    h.apply(Command::DownloadUpdated { download: download(3, Some(p), DownloadState::InProgress, 1) });
    let fx = h.apply(Command::UnloadTab { id: p });
    assert!(!has(&fx, |e| is_destroy(e, p)));
    assert!(!h.store.is_loaded(p));
    let fx = h.apply(Command::DownloadUpdated { download: download(3, Some(p), DownloadState::Cancelled, 1) });
    assert!(has(&fx, |e| is_destroy(e, p)));
    assert!(h.toast().is_none_or(|t| !t.message.contains("f3")));
}

#[test]
fn download_controls_dismiss_and_retry() {
    let mut h = Harness::new();
    let a = h.open("https://a.com/");
    h.apply(Command::DownloadUpdated { download: download(1, Some(a), DownloadState::InProgress, 1) });
    h.apply(Command::DownloadUpdated { download: download(2, None, DownloadState::Interrupted, 1) });
    assert_eq!(h.ui().downloads.iter().map(|d| d.id).collect::<Vec<_>>(), [2, 1], "newest first");
    assert_eq!(h.apply(Command::DownloadControl { id: 1, action: DownloadAction::Pause }), vec![Effect::DownloadControl { id: 1, action: DownloadAction::Pause }]);
    assert!(h.apply(Command::DownloadControl { id: 9, action: DownloadAction::Cancel }).is_empty());
    // In-progress downloads can't be dismissed.
    h.apply(Command::DownloadDismiss { id: 1 });
    assert_eq!(h.ui().downloads.len(), 2);
    // Retry an interrupted download: restarted in a live tab, old row removed.
    let fx = h.apply(Command::DownloadControl { id: 2, action: DownloadAction::Retry });
    assert_eq!(fx.iter().filter(|e| matches!(e, Effect::StartDownload { .. })).count(), 1);
    assert!(has(&fx, |e| matches!(e, Effect::StartDownload { tab, url } if *tab == a && url == "https://files.example.com/f2.zip")));
    assert_eq!(h.ui().downloads.len(), 1);
    h.apply(Command::DownloadUpdated { download: download(1, Some(a), DownloadState::Complete, 1000) });
    h.apply(Command::DownloadDismiss { id: 1 });
    assert!(h.ui().downloads.is_empty());
    // No live tab to retry in.
    let mut h = Harness::new();
    h.apply(Command::DownloadUpdated { download: download(5, None, DownloadState::Interrupted, 1) });
    assert!(!has(&h.apply(Command::DownloadControl { id: 5, action: DownloadAction::Retry }), |e| matches!(e, Effect::StartDownload { .. })));
    assert_eq!(h.toast().unwrap().message, "Open a tab to retry the download");
    // Sanitized values.
    let mut d = download(6, None, DownloadState::InProgress, -5);
    d.total_bytes = Some(0);
    d.bytes_per_sec = -1;
    h.apply(Command::DownloadUpdated { download: d });
    let d = h.ui().downloads[0].clone();
    assert_eq!((d.received_bytes, d.total_bytes, d.bytes_per_sec), (0, None, 0));
}

// ------------------------------------------------------------------------------------ permissions

#[test]
fn permission_prompts_queue_and_remember() {
    let mut h = Harness::new();
    let bg = h.open("https://bg.example.com/");
    let a = h.open("https://meet.example.com/room");
    let kinds = vec![PermissionKind::Camera, PermissionKind::Microphone];
    let fx = h.apply(Command::PermissionRequested { id: 1, tab: a, origin: "https://Meet.Example.com/".into(), kinds: kinds.clone() });
    assert_eq!(fx, vec![Effect::ShowPermissionPrompt { tab: a }]);
    let prompt = h.ui().permission_prompts[0].clone();
    assert_eq!((prompt.id, prompt.origin.as_str(), prompt.host.as_str()), (1, "https://meet.example.com", "meet.example.com"));
    // A prompt for a hidden tab queues without showing.
    let fx = h.apply(Command::PermissionRequested { id: 2, tab: bg, origin: "https://bg.example.com".into(), kinds: vec![PermissionKind::Notifications] });
    assert!(fx.is_empty());
    assert_eq!(h.ui().permission_prompts.len(), 2);
    // Allow + remember.
    let fx = h.apply(Command::ResolvePermission { id: 1, allow: true, remember: true });
    assert_eq!(fx, vec![Effect::HidePermissionPrompt, Effect::AnswerPermission { id: 1, allow: true, remember: true }].into_iter().rev().collect::<Vec<_>>());
    assert_eq!(h.store.state().site_permissions.len(), 2);
    // Remembered: answered immediately.
    assert_eq!(
        h.apply(Command::PermissionRequested { id: 3, tab: a, origin: "https://meet.example.com".into(), kinds: vec![PermissionKind::Camera] }),
        vec![Effect::AnswerPermission { id: 3, allow: true, remember: true }]
    );
    // Partially remembered: prompt.
    let fx = h.apply(Command::PermissionRequested { id: 4, tab: a, origin: "https://meet.example.com".into(), kinds: vec![PermissionKind::Camera, PermissionKind::Geolocation] });
    assert_eq!(fx, vec![Effect::ShowPermissionPrompt { tab: a }]);
    // Block + remember (a lasting deny), then a remembered deny wins.
    let fx = h.apply(Command::ResolvePermission { id: 4, allow: false, remember: true });
    assert!(fx.contains(&Effect::AnswerPermission { id: 4, allow: false, remember: true }), "{fx:?}");
    assert_eq!(
        h.apply(Command::PermissionRequested { id: 5, tab: a, origin: "https://meet.example.com".into(), kinds: vec![PermissionKind::Camera, PermissionKind::Geolocation] }),
        vec![Effect::AnswerPermission { id: 5, allow: false, remember: true }]
    );
    assert_eq!(h.store.state().site_permissions.iter().filter(|p| p.kind == PermissionKind::Camera).count(), 1, "one decision per kind");
    // Switching to the hidden tab shows its queued prompt.
    let fx = h.apply(Command::ActivateItem { id: bg });
    assert!(has(&fx, |e| matches!(e, Effect::ShowPermissionPrompt { tab } if *tab == bg)));
    // CEF dismissed it.
    let fx = h.apply(Command::PermissionDismissed { id: 2 });
    assert_eq!(fx, vec![Effect::HidePermissionPrompt]);
    assert!(h.ui().permission_prompts.is_empty());
    assert!(h.apply(Command::ResolvePermission { id: 2, allow: true, remember: false }).is_empty());
    // Closing a tab drops its prompts; unknown tabs are denied.
    h.apply(Command::PermissionRequested { id: 6, tab: bg, origin: "https://bg.example.com".into(), kinds: vec![PermissionKind::Clipboard] });
    let fx = h.apply(Command::CloseItem { id: Some(bg) });
    assert!(has(&fx, |e| matches!(e, Effect::HidePermissionPrompt)));
    assert!(h.ui().permission_prompts.is_empty());
    assert_eq!(
        h.apply(Command::PermissionRequested { id: 7, tab: 4242, origin: "https://x.com".into(), kinds: vec![PermissionKind::Camera] }),
        vec![Effect::AnswerPermission { id: 7, allow: false, remember: false }]
    );
}

#[test]
fn block_without_remember_is_a_one_off_answer() {
    let mut h = Harness::new();
    let a = h.open("https://maps.example.com/");
    h.apply(Command::PermissionRequested { id: 1, tab: a, origin: "https://maps.example.com".into(), kinds: vec![PermissionKind::Geolocation] });
    // Block without Remember: answered as not remembered (the shell dismisses, Chromium asks
    // again next time) and nothing is stored.
    let fx = h.apply(Command::ResolvePermission { id: 1, allow: false, remember: false });
    assert!(fx.contains(&Effect::AnswerPermission { id: 1, allow: false, remember: false }), "{fx:?}");
    assert!(h.store.state().site_permissions.is_empty());
    // The next request prompts again.
    let fx = h.apply(Command::PermissionRequested { id: 2, tab: a, origin: "https://maps.example.com".into(), kinds: vec![PermissionKind::Geolocation] });
    assert_eq!(fx, vec![Effect::ShowPermissionPrompt { tab: a }]);
    // Allow without Remember is a plain allow.
    let fx = h.apply(Command::ResolvePermission { id: 2, allow: true, remember: false });
    assert!(fx.contains(&Effect::AnswerPermission { id: 2, allow: true, remember: false }), "{fx:?}");
    // A duplicate request id is refused without a lasting deny.
    h.apply(Command::PermissionRequested { id: 3, tab: a, origin: "https://maps.example.com".into(), kinds: vec![PermissionKind::Geolocation] });
    let fx = h.apply(Command::PermissionRequested { id: 3, tab: a, origin: "https://maps.example.com".into(), kinds: vec![PermissionKind::Geolocation] });
    assert_eq!(fx, vec![Effect::AnswerPermission { id: 3, allow: false, remember: false }]);
}

// ------------------------------------------------------------------------------------ find & zoom

#[test]
fn find_bar_flow() {
    let mut h = Harness::new();
    assert!(h.apply(Command::OpenFind).is_empty(), "no tab");
    let a = h.open("https://a.com/");
    let b = h.open("https://b.com/");
    let fx = h.apply(Command::OpenFind);
    assert_eq!(fx, vec![Effect::ShowFindBar { tab: b }]);
    let f = h.ui().find.unwrap();
    assert_eq!((f.tab, f.text.as_str(), f.match_case), (b, "", false));
    let fx = h.apply(Command::FindInPage { tab: None, text: "needle".into(), forward: true, match_case: true, find_next: false });
    assert_eq!(fx, vec![Effect::Find { tab: b, text: "needle".into(), forward: true, match_case: true, find_next: false }]);
    assert_eq!(h.ui().find.unwrap().text, "needle");
    assert_eq!(h.apply(Command::FindNext { forward: false }), vec![Effect::Find { tab: b, text: "needle".into(), forward: false, match_case: true, find_next: true }]);
    // Re-open: new seq, prefilled.
    let seq = h.ui().find.unwrap().seq;
    assert_eq!(h.apply(Command::OpenFind), vec![Effect::ShowFindBar { tab: b }]);
    let f = h.ui().find.unwrap();
    assert!(f.seq > seq);
    assert_eq!(f.text, "needle");
    // Empty text stops finding.
    assert_eq!(h.apply(Command::FindInPage { tab: None, text: "".into(), forward: true, match_case: false, find_next: false }), vec![Effect::StopFinding { tab: b }]);
    h.apply(Command::FindInPage { tab: None, text: "x".into(), forward: true, match_case: false, find_next: false });
    let fx = h.apply(Command::CloseFind);
    assert_eq!(fx, vec![Effect::StopFinding { tab: b }, Effect::HideFindBar, Effect::FocusBrowser { tab: b }]);
    assert!(h.apply(Command::CloseFind).is_empty());
    // The query is remembered per tab while it stays loaded.
    h.apply(Command::OpenFind);
    assert_eq!(h.ui().find.unwrap().text, "x");
    // Switching tabs closes the bar.
    let fx = h.apply(Command::ActivateItem { id: a });
    assert!(has(&fx, |e| matches!(e, Effect::StopFinding { tab } if *tab == b)));
    assert!(has(&fx, |e| matches!(e, Effect::HideFindBar)));
    assert!(h.ui().find.is_none());
    // FindNext without a remembered query does nothing.
    assert!(h.apply(Command::FindNext { forward: true }).is_empty());
}

#[test]
fn zoom_steps_and_percent() {
    assert_eq!(store::zoom_percent(0.0), 100);
    assert_eq!(store::zoom_percent(1.0), 120);
    assert_eq!(store::zoom_percent(-1.0), 83);
    assert_eq!(store::zoom_percent((1.1f64).ln() / (1.2f64).ln()), 110);
    assert_eq!(store::zoom_percent((0.25f64).ln() / (1.2f64).ln()), 25);
    assert_eq!(store::zoom_percent(f64::NAN), 100);
    let mut h = Harness::new();
    assert!(h.apply(Command::Zoom { direction: ZoomDirection::In }).is_empty());
    let a = h.open("https://a.com/");
    assert_eq!(h.ui().current.unwrap().zoom_percent, 100);
    assert_eq!(h.apply(Command::Zoom { direction: ZoomDirection::In }), vec![Effect::Zoom { tab: a, direction: ZoomDirection::In }]);
    h.apply(Command::TabZoomChanged { tab: a, level: (1.1f64).ln() / (1.2f64).ln() });
    assert_eq!(h.ui().current.unwrap().zoom_percent, 110);
    assert_eq!(h.toast().unwrap().message, "Zoom 110%");
    h.apply(Command::DismissToast { id: h.toast().unwrap().id });
    // Zoom reported on load: no toast.
    h.apply(Command::TabZoomChanged { tab: a, level: 0.0 });
    assert_eq!(h.ui().current.unwrap().zoom_percent, 100);
    assert!(h.toast().is_none());
    assert!(h.apply(Command::TabZoomChanged { tab: a, level: f64::INFINITY }).is_empty());
}

#[test]
fn page_fullscreen() {
    let mut h = Harness::new();
    let a = h.open("https://a.com/");
    let b = h.open("https://video.com/");
    let fx = h.apply(Command::TabFullscreenChanged { tab: b, fullscreen: true });
    assert!(has(&fx, |e| matches!(e, Effect::SetPageFullscreen { tab: Some(t) } if *t == b)));
    assert!(h.ui().page_fullscreen);
    let toast = h.toast().unwrap();
    assert_eq!((toast.message.as_str(), toast.duration_ms), ("Press Esc to exit full screen", 3000));
    assert!(!has(&h.apply(Command::TabFullscreenChanged { tab: b, fullscreen: true }), |e| matches!(e, Effect::SetPageFullscreen { .. })));
    let fx = h.apply(Command::TabFullscreenChanged { tab: b, fullscreen: false });
    assert_eq!(fx, vec![Effect::SetPageFullscreen { tab: None }]);
    // Leaving the tab exits page fullscreen.
    h.apply(Command::TabFullscreenChanged { tab: b, fullscreen: true });
    let fx = h.apply(Command::ActivateItem { id: a });
    assert!(has(&fx, |e| matches!(e, Effect::ExitPageFullscreen { tab } if *tab == b)));
    assert!(has(&fx, |e| matches!(e, Effect::SetPageFullscreen { tab: None })));
    assert!(!h.ui().page_fullscreen);
    // A hidden tab can't go fullscreen.
    assert_eq!(h.apply(Command::TabFullscreenChanged { tab: b, fullscreen: true }), vec![Effect::ExitPageFullscreen { tab: b }]);
    // Window controls pass through.
    assert_eq!(h.apply(Command::WindowControl { action: WindowAction::ToggleFullscreen }), vec![Effect::Window { action: WindowAction::ToggleFullscreen }]);
}

// ------------------------------------------------------------------------------------ command bar

/// "Press it again to put it away": the keyboard sends the toggling forms, and each closes exactly
/// what the same key opened — never a bar in another mode, never a find bar of another tab.
#[test]
fn the_shortcut_that_opened_something_closes_it() {
    let mut h = Harness::new();
    // Ctrl+L with no tab opens the bar as NewTab, and Ctrl+L again closes that bar.
    h.apply(Command::ToggleCommandBar { mode: CommandBarMode::EditUrl });
    assert_eq!(h.ui().command_bar.unwrap().mode, CommandBarMode::NewTab);
    h.apply(Command::ToggleCommandBar { mode: CommandBarMode::EditUrl });
    assert!(h.ui().command_bar.is_none());

    let a = h.open("https://a.com/");
    // Ctrl+T, Ctrl+T.
    h.apply(Command::ToggleCommandBar { mode: CommandBarMode::NewTab });
    assert_eq!(h.ui().command_bar.unwrap().mode, CommandBarMode::NewTab);
    h.apply(Command::ToggleCommandBar { mode: CommandBarMode::NewTab });
    assert!(h.ui().command_bar.is_none());
    // Ctrl+T, then Ctrl+E: another mode re-targets the bar; Ctrl+E again closes it.
    h.apply(Command::ToggleCommandBar { mode: CommandBarMode::NewTab });
    h.apply(Command::ToggleCommandBar { mode: CommandBarMode::Extensions });
    assert_eq!(h.ui().command_bar.unwrap().mode, CommandBarMode::Extensions);
    h.apply(Command::ToggleCommandBar { mode: CommandBarMode::Extensions });
    assert!(h.ui().command_bar.is_none());
    // A button (`OpenCommandBar`) always opens.
    h.apply(Command::OpenCommandBar { mode: CommandBarMode::NewTab, split_side: None });
    h.apply(Command::OpenCommandBar { mode: CommandBarMode::NewTab, split_side: None });
    assert!(h.ui().command_bar.is_some());
    h.apply(Command::CloseCommandBar { seq: None });

    // Ctrl+F, Ctrl+F.
    h.apply(Command::ToggleFind);
    assert_eq!(h.ui().find.unwrap().tab, a);
    let fx = h.apply(Command::ToggleFind);
    assert!(h.ui().find.is_none());
    assert!(has(&fx, |e| matches!(e, Effect::StopFinding { tab } if *tab == a)), "{fx:?}");
    // A find bar left open on another tab is re-targeted, not closed.
    h.apply(Command::ToggleFind);
    let b = h.open("https://b.com/");
    h.apply(Command::ToggleFind);
    assert_eq!(h.ui().find.unwrap().tab, b);

    // Ctrl+, shows Settings; Ctrl+, again closes that tab — and only when it is the one in front.
    h.apply(Command::ToggleInternalPage { page: InternalPage::Settings });
    let settings = h.focused().expect("the settings tab");
    assert!(h.tab(settings).url.starts_with("sta://settings"));
    h.apply(Command::ActivateItem { id: b });
    h.apply(Command::ToggleInternalPage { page: InternalPage::Settings });
    assert_eq!(h.focused(), Some(settings), "from another tab the key goes to the page, it does not close it");
    h.apply(Command::ToggleInternalPage { page: InternalPage::Settings });
    assert!(!h.today().contains(&settings), "{:?}", h.today());

    // Ctrl+D, Ctrl+D: both directions say what happened.
    h.apply(Command::ActivateItem { id: a });
    h.apply(Command::TogglePin { id: None });
    assert!(h.pinned().contains(&a));
    assert_eq!(h.toast().map(|t| t.message), Some("Pinned".into()));
    h.apply(Command::TogglePin { id: None });
    assert!(h.today().contains(&a) && !h.pinned().contains(&a));
    assert_eq!(h.toast().map(|t| t.message), Some("Unpinned".into()));
}

#[test]
fn command_bar_modes_and_commit_semantics() {
    let mut h = Harness::new();
    // EditUrl without a tab behaves like NewTab.
    let fx = h.apply(Command::OpenCommandBar { mode: CommandBarMode::EditUrl, split_side: None });
    assert_eq!(fx, vec![Effect::ShowCommandBar]);
    let bar = h.ui().command_bar.unwrap();
    assert_eq!((bar.mode, bar.text.as_str()), (CommandBarMode::NewTab, ""));
    // Commit a typed URL: tab opens, bar hides, the page is focused afterwards.
    let fx = h.apply(Command::CommitOmnibox { command: Box::new(Command::OpenInput { text: "a.com".into(), target: OpenTarget::NewTab }), alt: false });
    let a = h.focused().unwrap();
    let hide = position(&fx, |e| matches!(e, Effect::HideCommandBar)).expect("hide");
    let focus = position(&fx, |e| matches!(e, Effect::FocusBrowser { tab } if *tab == a)).expect("focus");
    assert!(hide < focus);
    assert!(h.ui().command_bar.is_none());
    h.commit(a, "https://a.com/", "A");
    assert_eq!(h.store.history().get("https://a.com/").unwrap().typed_count, 1);
    // EditUrl prefills the focused tab's URL; re-opening bumps seq and re-shows.
    h.apply(Command::OpenCommandBar { mode: CommandBarMode::EditUrl, split_side: None });
    let bar = h.ui().command_bar.unwrap();
    assert_eq!((bar.mode, bar.text.as_str()), (CommandBarMode::EditUrl, "https://a.com/"));
    assert_eq!(h.apply(Command::OpenCommandBar { mode: CommandBarMode::EditUrl, split_side: None }), vec![Effect::ShowCommandBar]);
    assert!(h.ui().command_bar.unwrap().seq > bar.seq);
    // No FocusBrowser while the bar is open.
    assert!(!has(&h.apply(Command::ActivateItem { id: a }), |e| matches!(e, Effect::FocusBrowser { .. })));
    // Alt+Enter keeps the bar open.
    let fx = h.apply(Command::CommitOmnibox { command: Box::new(Command::OpenInput { text: "b.com".into(), target: OpenTarget::BackgroundTab }), alt: true });
    assert!(!has(&fx, |e| matches!(e, Effect::HideCommandBar)));
    assert!(h.ui().command_bar.is_some());
    assert_eq!(h.active(), Some(a));
    assert_eq!(h.today().len(), 2);
    let bg = h.today()[0];
    assert_ne!(bg, a, "a background tab without opener goes to the top of Today");
    assert!(h.store.is_loaded(bg));
    // A committed command that opens a bar mode keeps it (new seq).
    let seq = h.ui().command_bar.unwrap().seq;
    let fx = h.apply(Command::CommitOmnibox { command: Box::new(Command::OpenCommandBar { mode: CommandBarMode::Actions, split_side: None }), alt: false });
    assert_eq!(fx, vec![Effect::ShowCommandBar]);
    let bar = h.ui().command_bar.unwrap();
    assert!(bar.mode == CommandBarMode::Actions && bar.seq > seq);
    // …or a sidebar panel.
    h.apply(Command::CommitOmnibox { command: Box::new(Command::OpenSidebarPanel { panel: SidebarPanel::NewSpace }), alt: false });
    assert!(h.ui().command_bar.is_some());
    assert_eq!(h.ui().sidebar_panel.unwrap().panel, SidebarPanel::NewSpace);
    // Shell events and nested commits are rejected (bar stays).
    assert!(h.apply(Command::CommitOmnibox { command: Box::new(Command::Tick), alt: false }).is_empty());
    assert!(h.ui().command_bar.is_some());
    // An action that does nothing visible still closes the bar and refocuses the page.
    let fx = h.apply(Command::CommitOmnibox { command: Box::new(Command::CopyUrl { id: None, markdown: false }), alt: false });
    assert!(has(&fx, |e| matches!(e, Effect::HideCommandBar)));
    assert!(has(&fx, |e| matches!(e, Effect::FocusBrowser { tab } if *tab == a)));
    // Esc.
    h.apply(Command::OpenCommandBar { mode: CommandBarMode::NewTab, split_side: None });
    assert_eq!(h.apply(Command::CloseCommandBar { seq: None }), vec![Effect::HideCommandBar, Effect::FocusBrowser { tab: a }]);
    assert!(h.apply(Command::CloseCommandBar { seq: None }).is_empty());
    // Clicking into a page closes it (no FocusBrowser: the page already has focus).
    h.apply(Command::OpenCommandBar { mode: CommandBarMode::NewTab, split_side: None });
    assert_eq!(h.apply(Command::TabFocused { tab: a }), vec![Effect::HideCommandBar]);
    // Split mode commit through the omnibox.
    h.apply(Command::OpenCommandBar { mode: CommandBarMode::Split, split_side: Some(SplitSide::Left) });
    h.apply(Command::CommitOmnibox { command: Box::new(Command::SplitOpenInput { text: "rust docs".into(), side: SplitSide::Left }), alt: false });
    let sid = h.active().unwrap();
    let panes = h.split(sid).panes;
    assert_eq!(panes[1], a);
    assert_eq!(h.tab(panes[0]).url, "https://www.google.com/search?q=rust%20docs");
    assert!(h.ui().command_bar.is_none());
    // Startup in NewTab mode opens the bar.
    h.apply(Command::UpdateSettings { patch: SettingsPatch { startup: Some(Startup::NewTab), ..Default::default() } });
    let (store, _) = Store::load(Some(&h.store.state_json()), None, h.now);
    let h2 = Harness::start(store, Vec::new());
    assert_eq!(h2.ui().command_bar.map(|b| b.mode), Some(CommandBarMode::NewTab));
    assert_eq!(h2.store.content_layout(), ContentLayout::Empty);
    assert!(h2.history.contains(&Effect::ShowCommandBar));
}

#[test]
fn toasts_have_ids_and_durations() {
    let mut h = Harness::new();
    let a = h.open("https://a.com/");
    let fx = h.apply(Command::CopyText { text: "hello".into() });
    assert_eq!(fx, vec![Effect::CopyToClipboard { text: "hello".into() }, Effect::ShowToast]);
    let t1 = h.toast().unwrap();
    assert_eq!((t1.message.as_str(), t1.duration_ms, t1.action.is_none()), ("Copied", 2500, true));
    // A new toast replaces the current one (new id, re-shown).
    assert!(has(&h.apply(Command::TogglePin { id: Some(a) }), |e| matches!(e, Effect::ShowToast)));
    let t2 = h.toast().unwrap();
    assert!(t2.id > t1.id);
    // Dismissing a stale id does nothing.
    assert!(h.apply(Command::DismissToast { id: t1.id }).is_empty());
    assert_eq!(h.apply(Command::DismissToast { id: t2.id }), vec![Effect::HideToast]);
    assert!(h.toast().is_none());
    // Copy URL variants.
    h.apply(Command::TogglePin { id: Some(a) });
    h.apply(Command::TabAddressChanged { tab: a, url: "https://a.com/p?utm_medium=x&fbclid=y&k=v".into() });
    h.apply(Command::TabTitleChanged { tab: a, title: "A [draft]".into() });
    assert!(has(&h.apply(Command::CopyUrl { id: None, markdown: false }), |e| matches!(e, Effect::CopyToClipboard { text } if text == "https://a.com/p?k=v")));
    assert_eq!(h.toast().unwrap().message, "Copied URL");
    assert!(has(&h.apply(Command::CopyUrl { id: Some(a), markdown: true }), |e| matches!(e, Effect::CopyToClipboard { text } if text == "[A \\[draft\\]](https://a.com/p?k=v)")));
    assert_eq!(h.toast().unwrap().message, "Copied URL as Markdown");
    assert!(h.apply(Command::CopyUrl { id: Some(999), markdown: false }).is_empty());
}

#[test]
fn mru_switcher_first_step_when_the_focused_tab_is_not_mru_head() {
    let mut h = Harness::new();
    let a = h.open("https://a.com/");
    let b = h.open("https://b.com/");
    // An empty space: nothing is focused, so MRU[0] (b) is already "the previous tab".
    h.new_space("Empty", "🫙", Theme::default());
    assert_eq!(h.focused(), None);
    h.apply(Command::MruStep { forward: true });
    let sw = h.ui().switcher.unwrap();
    assert_eq!((sw.tabs.iter().map(|t| t.id).collect::<Vec<_>>(), sw.selected), (vec![b, a], 0));
    h.apply(Command::MruCommit);
    assert_eq!(h.focused(), Some(b));
    // Backwards from the empty state selects the oldest card.
    h.new_space("Empty 2", "🫙", Theme::default());
    h.apply(Command::MruStep { forward: false });
    assert_eq!(h.ui().switcher.unwrap().selected, 1);
    h.apply(Command::MruCancel);
    // With Peek open the focus is the Peek, so the first card is the tab behind it.
    h.apply(Command::ActivateItem { id: a });
    h.apply(Command::LinkOpenRequested { opener: a, url: "https://peek.org/".into(), disposition: LinkDisposition::NewWindow });
    assert!(h.store.peek_tab().is_some());
    h.apply(Command::MruStep { forward: true });
    assert_eq!(h.ui().switcher.unwrap().selected, 0);
    h.apply(Command::MruCommit);
    assert!(h.store.peek_tab().is_none());
    assert_eq!(h.focused(), Some(a));
    // A single other tab is enough when nothing is focused.
    let mut h = Harness::new();
    let solo = h.open("https://solo.com/");
    assert!(h.apply(Command::MruStep { forward: true }).is_empty(), "only the focused tab");
    h.new_space("Empty", "🫙", Theme::default());
    h.apply(Command::MruStep { forward: true });
    assert_eq!(h.ui().switcher.unwrap().selected, 0);
    h.apply(Command::MruCommit);
    assert_eq!(h.focused(), Some(solo));
}
