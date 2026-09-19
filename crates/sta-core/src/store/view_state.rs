//! `UiState` and page list builders.

use super::*;
use crate::urls;

/// Precomputed context shared by all tab views of one snapshot.
struct ViewCtx {
    layout: Vec<Id>,
    focused: Option<Id>,
    peek: Option<Id>,
}

impl Store {
    fn tab_view(&self, ctx: &ViewCtx, tab: &Tab, section: Option<Section>, space: Option<Id>) -> TabView {
        let r = self.rt.tabs.get(&tab.id);
        let live = self.is_live(tab.id);
        TabView {
            id: tab.id,
            title: urls::display_title(tab.custom_title.as_deref(), &tab.title, &tab.url),
            url: tab.url.clone(),
            host: urls::display_host(&tab.url),
            favicon: tab.favicon.clone(),
            section,
            space,
            loaded: self.is_loaded(tab.id),
            loading: live && r.is_some_and(|r| r.loading),
            audible: live && r.is_some_and(|r| r.audible),
            muted: tab.muted,
            crashed: live && r.is_some_and(|r| r.crashed),
            failed: live && r.is_some_and(|r| r.load_error.is_some()),
            navigated: tab.pinned_url.as_deref().is_some_and(|p| urls::differs_from_pinned(&tab.url, p)),
            pinned_url: tab.pinned_url.clone(),
            active: ctx.focused == Some(tab.id),
            visible: ctx.layout.contains(&tab.id) || ctx.peek == Some(tab.id),
            agent: self.rt.agent.tabs.contains(&tab.id),
        }
    }

    fn node_view(&self, ctx: &ViewCtx, id: Id, section: Section, space: Id, depth: usize) -> Option<NodeView> {
        match self.state.items.get(&id)? {
            Item::Tab(t) => Some(NodeView::Tab(self.tab_view(ctx, t, Some(section), Some(space)))),
            Item::Folder(f) => Some(NodeView::Folder(FolderView {
                id: f.id,
                name: f.name.clone(),
                collapsed: f.collapsed,
                children: if depth > 64 {
                    Vec::new()
                } else {
                    f.children.iter().filter_map(|c| self.node_view(ctx, *c, section, space, depth + 1)).collect()
                },
            })),
            Item::Split(s) => Some(NodeView::Split(SplitView {
                id: s.id,
                orientation: s.orientation,
                panes: s.panes.iter().filter_map(|p| self.tab_item(*p)).map(|t| self.tab_view(ctx, t, Some(Section::Today), Some(space))).collect(),
                fractions: s.fractions.clone(),
                focused: s.focused,
                active: self.active_item() == Some(s.id),
            })),
        }
    }

    pub(super) fn build_ui_state(&self) -> UiState {
        let dark = self.is_dark();
        let ctx = ViewCtx { layout: self.layout_tab_ids(), focused: self.content_focused_tab(), peek: self.peek_tab() };
        let spaces = self
            .state
            .spaces
            .iter()
            .map(|s| SpaceView {
                id: s.id,
                name: s.name.clone(),
                icon: s.icon.clone(),
                theme: s.theme.clone(),
                colors: crate::theme::colors(&s.theme, dark),
                pinned: s.pinned.iter().filter_map(|i| self.node_view(&ctx, *i, Section::Pinned, s.id, 0)).collect(),
                today: s.today.iter().filter_map(|i| self.node_view(&ctx, *i, Section::Today, s.id, 0)).collect(),
                active_item: s.active_item,
            })
            .collect();
        let favorites = self
            .state
            .favorites
            .iter()
            .filter_map(|f| self.tab_item(*f))
            .map(|t| self.tab_view(&ctx, t, Some(Section::Favorites), None))
            .collect();
        let peek = self.rt.peek.as_ref().map(|p| PeekView { tab: self.tab_view(&ctx, &p.tab, None, None), popup: p.popup });
        let switcher = self.rt.switcher.as_ref().map(|(tabs, selected)| SwitcherView {
            tabs: tabs
                .iter()
                .filter_map(|t| self.tab_item(*t))
                .map(|t| self.tab_view(&ctx, t, self.section_of(t.id), self.space_of(t.id)))
                .collect(),
            selected: *selected,
        });
        let mut prompts = self.rt.permission_prompts.clone();
        if let Some((shown, _)) = self.shown_prompt()
            && let Some(i) = prompts.iter().position(|p| p.id == shown)
        {
            let p = prompts.remove(i);
            prompts.insert(0, p);
        }
        UiState {
            revision: self.revision,
            archive_revision: self.rt.archive_revision,
            history_revision: self.rt.history_revision,
            dark,
            window: WindowView {
                maximized: self.rt.window_maximized,
                fullscreen: self.rt.window_fullscreen,
                focused: self.rt.window_focused,
                sidebar_visible: self.sidebar_docked(),
                sidebar_width: self.state.window.sidebar_width,
            },
            active_space: self.state.window.active_space,
            spaces,
            favorites,
            favorites_full: self.state.favorites.len() >= MAX_FAVORITES,
            active_item: self.active_item(),
            focused_tab: self.focused_tab(),
            current: self.current_view(),
            peek,
            downloads: self.rt.downloads.iter().take(20).cloned().collect(),
            can_reopen: self.can_reopen(),
            archive_count: self.state.archive.len(),
            switcher,
            command_bar: self.rt.command_bar.clone(),
            find: self.rt.find.clone(),
            toast: self.rt.toast.clone(),
            sidebar_panel: self.rt.sidebar_panel.clone(),
            permission_prompts: prompts,
            page_fullscreen: self.rt.page_fullscreen.is_some(),
            motion: self.motion_view(),
            update: self.rt.update.clone(),
            settings: self.state.settings.clone(),
            boosts: self.state.boosts.iter().map(boost_summary).collect(),
            search_engines: crate::omnibox::search_engines(&self.state.settings.custom_search_url),
            theme_presets: crate::theme::presets(dark),
            agent: self.agent_view(),
            extensions: self.extensions_view(),
        }
    }

    fn current_view(&self) -> Option<CurrentView> {
        let id = self.focused_tab()?;
        let tab = self.tab(id)?;
        let live = self.is_live(id);
        let r = self.rt.tabs.get(&id).filter(|_| live);
        let section = self.tab_section(id);
        Some(CurrentView {
            tab: id,
            url: tab.url.clone(),
            title: urls::display_title(tab.custom_title.as_deref(), &tab.title, &tab.url),
            host: urls::display_host(&tab.url),
            pill: urls::display_host(&tab.url),
            secure: urls::is_secure(&tab.url),
            internal: urls::is_internal(&tab.url),
            loading: r.is_some_and(|r| r.loading),
            progress: r.map_or(0.0, |r| if r.progress.is_finite() { r.progress } else { 0.0 }),
            can_go_back: r.is_some_and(|r| r.can_go_back),
            can_go_forward: r.is_some_and(|r| r.can_go_forward),
            section,
            navigated: tab.pinned_url.as_deref().is_some_and(|p| urls::differs_from_pinned(&tab.url, p)),
            zoom_percent: zoom_percent(self.rt.tabs.get(&id).map_or(0.0, |r| r.zoom_level)),
            boosts: self.state.boosts.iter().filter(|b| urls::host_matches(&b.host, &tab.url)).map(boost_summary).collect(),
            muted: tab.muted,
            audible: r.is_some_and(|r| r.audible),
            load_error: r.and_then(|r| r.load_error.clone()),
            split_panes: self.active_item().and_then(|a| self.split_item(a)).map_or(0, |s| s.panes.len()),
            translate: r.map(|r| r.translate.clone()).unwrap_or_default(),
        })
    }

    pub(super) fn build_archive_list(&self) -> Vec<ArchiveEntryView> {
        let mut rows: Vec<&ArchiveEntry> = self.state.archive.iter().collect();
        rows.sort_by_key(|e| std::cmp::Reverse(e.archived_at));
        rows.into_iter()
            .map(|e| {
                let space = e.space.and_then(|s| self.space(s));
                ArchiveEntryView {
                    id: e.id,
                    url: e.url.clone(),
                    title: urls::display_title(e.custom_title.as_deref(), &e.title, &e.url),
                    host: urls::display_host(&e.url),
                    favicon: e.favicon.clone(),
                    archived_at: e.archived_at,
                    reason: e.reason,
                    space: e.space,
                    space_icon: space.map(|s| s.icon.clone()),
                    space_name: space.map(|s| s.name.clone()),
                    group: e.split.as_ref().map(|s| s.group),
                }
            })
            .collect()
    }
}

fn boost_summary(b: &Boost) -> BoostSummary {
    BoostSummary { id: b.id, name: b.name.clone(), host: b.host.clone(), enabled: b.enabled }
}
