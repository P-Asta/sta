//! Debug invariant checker (used by tests after every command).

use super::*;

pub(super) fn check(store: &Store) -> Result<(), Vec<String>> {
    let mut errors: Vec<String> = Vec::new();
    let st = &store.state;
    let mut err = |s: String| errors.push(s);

    // ------------------------------------------------------------------ containers
    let mut seen: BTreeMap<Id, String> = BTreeMap::new();
    let mut note = |id: Id, place: String, err: &mut dyn FnMut(String)| {
        if let Some(prev) = seen.insert(id, place.clone()) {
            err(format!("item {id} listed in {prev} and {place}"));
        }
    };
    if st.favorites.len() > MAX_FAVORITES {
        err(format!("{} favorites > {MAX_FAVORITES}", st.favorites.len()));
    }
    for f in &st.favorites {
        note(*f, "favorites".into(), &mut err);
        match st.items.get(f) {
            Some(Item::Tab(t)) => {
                if t.pinned_url.is_none() {
                    err(format!("favorite {f} without pinnedUrl"));
                }
            }
            other => err(format!("favorite {f} is {other:?}")),
        }
    }
    if st.spaces.is_empty() {
        err("no spaces".into());
    }
    let mut space_ids = BTreeSet::new();
    for s in &st.spaces {
        if !space_ids.insert(s.id) {
            err(format!("duplicate space id {}", s.id));
        }
        if st.items.contains_key(&s.id) {
            err(format!("space id {} collides with an item", s.id));
        }
        let mut stack: Vec<(Id, usize, String)> = s.pinned.iter().map(|i| (*i, 1, format!("pinned of {}", s.id))).collect();
        while let Some((id, depth, place)) = stack.pop() {
            note(id, place, &mut err);
            match st.items.get(&id) {
                Some(Item::Tab(t)) => {
                    if t.pinned_url.is_none() {
                        err(format!("pinned tab {id} without pinnedUrl"));
                    }
                }
                Some(Item::Folder(f)) => {
                    if depth > MAX_FOLDER_DEPTH {
                        err(format!("folder {id} at depth {depth}"));
                    }
                    stack.extend(f.children.iter().map(|c| (*c, depth + 1, format!("folder {id}"))));
                }
                Some(Item::Split(_)) => err(format!("split {id} in Pinned")),
                None => err(format!("pinned reference to missing item {id}")),
            }
        }
        for id in &s.today {
            note(*id, format!("today of {}", s.id), &mut err);
            match st.items.get(id) {
                Some(Item::Tab(t)) => {
                    if t.pinned_url.is_some() {
                        err(format!("today tab {id} has pinnedUrl"));
                    }
                }
                Some(Item::Split(sp)) => {
                    if !(2..=MAX_SPLIT_PANES).contains(&sp.panes.len()) {
                        err(format!("split {id} has {} panes", sp.panes.len()));
                    }
                    if sp.fractions.len() != sp.panes.len() {
                        err(format!("split {id} fractions/panes length mismatch"));
                    }
                    if sp.fractions.iter().any(|f| !f.is_finite() || *f <= 0.0) {
                        err(format!("split {id} has invalid fractions {:?}", sp.fractions));
                    }
                    let sum: f32 = sp.fractions.iter().sum();
                    if (sum - 1.0).abs() > 0.01 {
                        err(format!("split {id} fractions sum {sum}"));
                    }
                    if sp.focused >= sp.panes.len().max(1) {
                        err(format!("split {id} focused {} out of range", sp.focused));
                    }
                    for p in &sp.panes {
                        note(*p, format!("split {id}"), &mut err);
                        match st.items.get(p) {
                            Some(Item::Tab(t)) => {
                                if t.pinned_url.is_some() {
                                    err(format!("pane {p} has pinnedUrl"));
                                }
                            }
                            other => err(format!("pane {p} of split {id} is {other:?}")),
                        }
                    }
                }
                Some(Item::Folder(_)) => err(format!("folder {id} in Today")),
                None => err(format!("today reference to missing item {id}")),
            }
        }
    }
    for id in st.items.keys() {
        if !seen.contains_key(id) {
            err(format!("orphan item {id}"));
        }
        if let Some(Item::Tab(t)) = st.items.get(id)
            && t.id != *id
        {
            err(format!("tab key {id} != id {}", t.id));
        }
    }
    // Every split must be listed by a Today container (panes by a split) — covered by `seen`.
    for (id, place) in &seen {
        if matches!(st.items.get(id), Some(Item::Split(_))) && !place.starts_with("today") {
            err(format!("split {id} listed in {place}"));
        }
        if matches!(st.items.get(id), Some(Item::Folder(_))) && place.starts_with("today") {
            err(format!("folder {id} listed in {place}"));
        }
    }

    // ------------------------------------------------------------------ ids
    let max_id = st
        .items
        .keys()
        .copied()
        .chain(st.spaces.iter().map(|s| s.id))
        .chain(st.boosts.iter().map(|b| b.id))
        .chain(st.archive.iter().map(|e| e.id))
        .chain(store.rt.peek.iter().map(|p| p.tab.id))
        .max()
        .unwrap_or(0);
    if st.next_id <= max_id {
        err(format!("nextId {} <= max id {max_id}", st.next_id));
    }
    let mut boost_ids = BTreeSet::new();
    for b in &st.boosts {
        if !boost_ids.insert(b.id) || b.id == 0 {
            err(format!("bad/duplicate boost id {}", b.id));
        }
    }
    let mut archive_ids = BTreeSet::new();
    for e in &st.archive {
        if !archive_ids.insert(e.id) {
            err(format!("duplicate archive id {}", e.id));
        }
        if st.items.contains_key(&e.id) {
            err(format!("archive entry {} collides with a live item", e.id));
        }
    }

    // ------------------------------------------------------------------ window & spaces
    if !st.spaces.iter().any(|s| s.id == st.window.active_space) {
        err(format!("active space {} missing", st.window.active_space));
    }
    for s in &st.spaces {
        if let Some(a) = s.active_item
            && !store.valid_active_for(s.id, a)
        {
            err(format!("space {} active item {a} invalid", s.id));
        }
        if !s.theme.hue.is_finite() || !s.theme.hue2.is_finite() || !s.theme.chroma.is_finite() {
            err(format!("space {} theme not finite", s.id));
        }
    }
    if st.window.mru.len() > MAX_MRU {
        err("mru too long".into());
    }
    let mut mru_seen = BTreeSet::new();
    for t in &st.window.mru {
        if !mru_seen.insert(*t) {
            err(format!("mru duplicate {t}"));
        }
        if !matches!(st.items.get(t), Some(Item::Tab(_))) {
            err(format!("mru entry {t} is not a tab"));
        }
    }
    if !(SIDEBAR_MIN_WIDTH..=SIDEBAR_MAX_WIDTH).contains(&st.window.sidebar_width) {
        err(format!("sidebar width {}", st.window.sidebar_width));
    }
    if st.reopen.len() > MAX_REOPEN_STACK {
        err("reopen stack too long".into());
    }

    // ------------------------------------------------------------------ runtime
    let rt = &store.rt;
    if let Some(p) = &rt.peek
        && st.items.contains_key(&p.tab.id)
    {
        err(format!("peek tab {} is also an item", p.tab.id));
    }
    for t in rt.closing.iter().chain(rt.deferred_destroy.iter()) {
        if !rt.tabs.get(t).is_some_and(|r| r.loaded) {
            err(format!("closing/deferred tab {t} not marked loaded"));
        }
    }
    for t in rt.pending_create.keys() {
        if !rt.closing.contains(t) {
            err(format!("pending create for {t} without pending close"));
        }
        if store.tab(*t).is_none() {
            err(format!("pending create for unknown tab {t}"));
        }
    }
    for (id, r) in &rt.tabs {
        if r.loaded && store.tab(*id).is_none() && !rt.closing.contains(id) && !rt.deferred_destroy.contains(id) {
            err(format!("live browser for unknown tab {id}"));
        }
        if !r.progress.is_finite() || !r.zoom_level.is_finite() {
            err(format!("tab {id} runtime floats not finite"));
        }
    }
    if rt.started {
        if let Some(layout) = &rt.emitted.layout {
            for t in layout.tabs() {
                if !store.is_live(t) {
                    err(format!("shown tab {t} is not live"));
                }
            }
            if *layout != store.desired_layout() {
                err(format!("shown layout {layout:?} != desired {:?}", store.desired_layout()));
            }
        }
        for t in store.layout_tab_ids() {
            if !store.is_live(t) && !rt.pending_create.contains_key(&t) {
                err(format!("visible tab {t} has no browser"));
            }
        }
        if let Some(p) = rt.peek.as_ref().map(|p| p.tab.id) {
            if !store.is_live(p) {
                err(format!("peek tab {p} has no browser"));
            }
            if rt.emitted.peek != Some(p) {
                err(format!("peek tab {p} not shown"));
            }
        }
        if let Some(f) = &rt.find
            && Some(f.tab) != store.focused_tab()
        {
            err(format!("find bar on unfocused tab {}", f.tab));
        }
    }
    if errors.is_empty() { Ok(()) } else { Err(errors) }
}
