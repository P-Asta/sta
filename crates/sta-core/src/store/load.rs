//! Tolerant loading of `state.json` / `history.json` and invariant repair.
//!
//! Parsing goes through `serde_json::Value` first: every item, space, archive entry, … is parsed
//! on its own, and structs fall back field by field (a field with a wrong type keeps its default),
//! so one bad value never loses the whole profile. `state_corrupt` / `history_corrupt` are set
//! when data had to be dropped because it could not be parsed (the shell quarantines the file);
//! structural repairs (orphans, duplicate ids, limits) only add warnings.

use super::tree::{equalize, normalize_fractions};
use super::*;
use crate::{MAX_ID, MAX_MILLIS};
use crate::history::{HistoryUrl, MAX_HISTORY_URLS, MAX_VISITS_PER_URL};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{Map, Value};

pub(super) fn load(state_json: Option<&str>, history_json: Option<&str>, now: Millis) -> (Store, LoadReport) {
    let mut report = LoadReport::default();
    let mut history_repaired = false;
    let history = match history_json {
        Some(text) => parse_history(text, now, &mut report, &mut history_repaired),
        None => History::default(),
    };
    let mut store = match state_json {
        None => Store::new(now),
        Some(text) => match serde_json::from_str::<Value>(text) {
            Ok(mut root @ Value::Object(_)) => {
                upgrade_legacy_urls(&mut root, "state", &mut report);
                let Value::Object(obj) = root else { unreachable!("matched an object") };
                let mut state = parse_state(&obj, &mut report);
                repair(&mut state, now, &mut report);
                let dirty = report.state_corrupt || !report.warnings.is_empty();
                Store::from_state(state, History::default(), DirtyFlags { state: dirty, history: false })
            }
            Ok(_) => {
                report.state_corrupt = true;
                report.warnings.push("state.json is not a JSON object; starting fresh".into());
                Store::new(now)
            }
            Err(e) => {
                report.state_corrupt = true;
                report.warnings.push(format!("state.json is not valid JSON ({e}); starting fresh"));
                Store::new(now)
            }
        },
    };
    store.history = history;
    if report.history_corrupt || history_repaired {
        store.dirty.history = true;
    }
    (store, report)
}

/// Rewrites internal URLs saved before the rename to `sta://…` ([`crate::legacy::upgrade_json`])
/// anywhere in a loaded file. A rewrite adds a warning, which marks the file
/// dirty so the upgraded form is saved.
fn upgrade_legacy_urls(root: &mut Value, what: &str, report: &mut LoadReport) -> bool {
    let n = crate::legacy::upgrade_json(root);
    if n > 0 {
        report.warnings.push(format!("{what}: {n} internal URL(s) from before the rename upgraded to sta://"));
    }
    n > 0
}

/// Deserialize `v` as `T`; on failure, merge it field by field into `T::default()` keeping
/// only the fields that deserialize.
fn tolerant<T: Serialize + DeserializeOwned + Default>(v: &Value, what: &str, report: &mut LoadReport) -> Option<T> {
    if let Ok(t) = serde_json::from_value::<T>(v.clone()) {
        return Some(t);
    }
    let Value::Object(obj) = v else {
        report.state_corrupt = true;
        report.warnings.push(format!("{what}: not an object, dropped"));
        return None;
    };
    let mut base = serde_json::to_value(T::default()).ok()?;
    let Value::Object(base_map) = &mut base else { return None };
    let mut bad = Vec::new();
    for (k, val) in obj {
        if !base_map.contains_key(k) {
            continue;
        }
        let prev = base_map.insert(k.clone(), val.clone());
        let ok = serde_json::from_value::<T>(Value::Object(base_map.clone())).is_ok();
        if !ok {
            match prev {
                Some(p) => base_map.insert(k.clone(), p),
                None => base_map.remove(k),
            };
            bad.push(k.clone());
        }
    }
    if !bad.is_empty() {
        report.state_corrupt = true;
        report.warnings.push(format!("{what}: invalid field(s) {} reset to defaults", bad.join(", ")));
    }
    serde_json::from_value::<T>(base).ok()
}

fn strict<T: DeserializeOwned>(v: &Value, what: &str, report: &mut LoadReport) -> Option<T> {
    match serde_json::from_value::<T>(v.clone()) {
        Ok(t) => Some(t),
        Err(e) => {
            report.state_corrupt = true;
            report.warnings.push(format!("{what}: dropped ({e})"));
            None
        }
    }
}

fn array<'a>(obj: &'a Map<String, Value>, key: &str, report: &mut LoadReport) -> &'a [Value] {
    match obj.get(key) {
        Some(Value::Array(a)) => a,
        Some(Value::Null) | None => &[],
        Some(_) => {
            report.state_corrupt = true;
            report.warnings.push(format!("{key}: not an array, dropped"));
            &[]
        }
    }
}

/// A usable id: `1..=MAX_ID`.
fn valid_id(id: Id) -> bool {
    (1..=MAX_ID).contains(&id)
}

/// An id as parsed: any non-zero `u64`. Ids above [`RENUMBER_ABOVE`] (including ones beyond
/// `MAX_ID`) are renumbered by `repair`, so they keep their references instead of being dropped.
fn id_value(v: &Value) -> Option<Id> {
    v.as_u64().filter(|id| *id != 0)
}

/// Loaded ids above this (half of [`MAX_ID`]) make `repair` renumber every id compactly, which
/// leaves at least 2^52 ids to allocate. Real profiles never get anywhere near it; only corrupt or
/// hand-edited ones do.
pub(crate) const RENUMBER_ABOVE: Id = MAX_ID / 2;

/// Clamp a loaded timestamp to `0..=MAX_MILLIS`, counting repairs.
fn clamp_time(t: &mut Millis, repaired: &mut usize) {
    let clamped = (*t).clamp(0, MAX_MILLIS);
    if clamped != *t {
        *t = clamped;
        *repaired += 1;
    }
}

fn parse_state(obj: &Map<String, Value>, report: &mut LoadReport) -> State {
    // An out-of-range nextId is recomputed from the ids in use (see `repair`).
    let next_id = match obj.get("nextId") {
        None | Some(Value::Null) => 1,
        Some(v) => v.as_u64().filter(|id| valid_id(*id)).unwrap_or_else(|| {
            report.warnings.push(format!("nextId {v} out of range; recomputed"));
            1
        }),
    };
    let mut state = State { next_id, ..State::default() };
    if let Some(v) = obj.get("settings") {
        state.settings = tolerant(v, "settings", report).unwrap_or_default();
    }
    migrate(obj, &mut state, report);
    if let Some(v) = obj.get("window") {
        state.window = tolerant(v, "window", report).unwrap_or_default();
    }
    for (i, v) in array(obj, "spaces", report).iter().enumerate() {
        if let Some(s) = tolerant::<Space>(v, &format!("spaces[{i}]"), report) {
            state.spaces.push(s);
        }
    }
    for v in array(obj, "favorites", report) {
        match id_value(v) {
            Some(id) => state.favorites.push(id),
            None => report.warnings.push(format!("favorites: invalid id {v} dropped")),
        }
    }
    match obj.get("items") {
        Some(Value::Object(items)) => {
            for (key, v) in items {
                let what = format!("items[{key}]");
                let kind = v.get("kind").and_then(Value::as_str);
                let item = match kind {
                    Some("tab") => tolerant::<Tab>(v, &what, report).map(Item::Tab),
                    Some("folder") => tolerant::<Folder>(v, &what, report).map(Item::Folder),
                    Some("split") => tolerant::<Split>(v, &what, report).map(Item::Split),
                    _ => {
                        report.state_corrupt = true;
                        report.warnings.push(format!("{what}: unknown kind, dropped"));
                        None
                    }
                };
                let Some(mut item) = item else { continue };
                let key_id = key.parse::<Id>().ok().filter(|k| *k != 0);
                let inner = item.id();
                let id = if inner != 0 { inner } else { key_id.unwrap_or(0) };
                if id == 0 {
                    report.warnings.push(format!("{what}: missing id, dropped"));
                    continue;
                }
                set_item_id(&mut item, id);
                if state.items.contains_key(&id) {
                    report.warnings.push(format!("{what}: duplicate id {id}, dropped"));
                    continue;
                }
                state.items.insert(id, item);
            }
        }
        None | Some(Value::Null) => {}
        Some(_) => {
            report.state_corrupt = true;
            report.warnings.push("items: not an object, dropped".into());
        }
    }
    for (i, v) in array(obj, "archive", report).iter().enumerate() {
        if let Some(e) = tolerant::<ArchiveEntry>(v, &format!("archive[{i}]"), report) {
            state.archive.push(e);
        }
    }
    for (i, v) in array(obj, "boosts", report).iter().enumerate() {
        if let Some(b) = tolerant::<Boost>(v, &format!("boosts[{i}]"), report) {
            state.boosts.push(b);
        }
    }
    for (i, v) in array(obj, "reopen", report).iter().enumerate() {
        if let Some(r) = strict::<ReopenEntry>(v, &format!("reopen[{i}]"), report) {
            state.reopen.push(r);
        }
    }
    for (i, v) in array(obj, "sitePermissions", report).iter().enumerate() {
        if let Some(p) = strict::<SitePermission>(v, &format!("sitePermissions[{i}]"), report) {
            state.site_permissions.push(p);
        }
    }
    // Externally registered extensions the user has already been told about (D11a): an id that is
    // not one is dropped in silence — this is bookkeeping, not content, and a bad entry would only
    // ever suppress one toast.
    for v in array(obj, "announcedExternal", report) {
        if let Some(id) = v.as_str().filter(|s| crate::urls::is_extension_id(s)) {
            state.announced_external.push(id.to_string());
        }
    }
    state.announced_external.sort();
    state.announced_external.dedup();
    state
}

/// Upgrades settings saved by an older version (a missing or invalid `version` counts as 0). The
/// report gets a note, so the upgraded profile is saved.
fn migrate(obj: &Map<String, Value>, state: &mut State, report: &mut LoadReport) {
    let version = obj.get("version").and_then(Value::as_u64).unwrap_or(0);
    if version >= u64::from(STATE_VERSION) {
        return;
    }
    // 2: search suggestions became functional (the setting was hidden and inert before).
    if version < 2 {
        state.settings.search_suggestions = true;
    }
    report.warnings.push(format!("state version {version} migrated to {STATE_VERSION} (search suggestions on)"));
}

fn set_item_id(item: &mut Item, id: Id) {
    match item {
        Item::Tab(t) => t.id = id,
        Item::Folder(f) => f.id = id,
        Item::Split(s) => s.id = id,
    }
}

/// Walks the containers and claims items for exactly one place.
struct Claimer {
    items: BTreeMap<Id, Item>,
    claimed: BTreeMap<Id, Item>,
    warnings: Vec<String>,
}

impl Claimer {
    fn take(&mut self, id: Id) -> Option<Item> {
        if self.claimed.contains_key(&id) {
            self.warnings.push(format!("item {id} listed more than once; later reference dropped"));
            return None;
        }
        let item = self.items.remove(&id);
        if item.is_none() {
            self.warnings.push(format!("reference to missing item {id} dropped"));
        }
        item
    }

    fn claim_tab(&mut self, id: Id) -> Option<Id> {
        match self.take(id)? {
            item @ Item::Tab(_) => {
                self.claimed.insert(id, item);
                Some(id)
            }
            other => {
                // Not a tab where only tabs are allowed: put it back for another container.
                self.warnings.push(format!("item {id} is not a tab here; moved"));
                self.items.insert(id, other);
                None
            }
        }
    }

    /// Split in Today: claims its panes. Returns the split id, the single remaining tab, or None.
    fn claim_split(&mut self, id: Id) -> Option<Id> {
        let Some(Item::Split(mut s)) = self.take(id) else { return None };
        let mut panes = Vec::new();
        let mut fractions = Vec::new();
        for (i, p) in s.panes.iter().enumerate() {
            if panes.len() >= MAX_SPLIT_PANES {
                self.warnings.push(format!("split {id}: more than {MAX_SPLIT_PANES} panes, extra dropped"));
                break;
            }
            let is_tab = matches!(self.items.get(p), Some(Item::Tab(_)));
            if is_tab && self.claim_tab(*p).is_some() {
                panes.push(*p);
                fractions.push(s.fractions.get(i).copied().unwrap_or(0.0));
            } else if !is_tab {
                self.warnings.push(format!("split {id}: pane {p} dropped"));
            }
        }
        match panes.len() {
            0 => {
                self.warnings.push(format!("split {id}: no panes, removed"));
                None
            }
            1 => {
                self.warnings.push(format!("split {id}: single pane, dissolved"));
                Some(panes[0])
            }
            _ => {
                if fractions.len() != panes.len() || fractions.iter().any(|f| !f.is_finite() || *f <= 0.0) {
                    fractions = vec![0.0; panes.len()];
                    equalize(&mut fractions);
                } else {
                    normalize_fractions(&mut fractions, 0.0);
                }
                s.focused = s.focused.min(panes.len() - 1);
                s.panes = panes;
                s.fractions = fractions;
                self.claimed.insert(id, Item::Split(s));
                Some(id)
            }
        }
    }

    /// Folder (in Pinned) at `depth`. Folders nested too deep are flattened into their parent.
    /// Returns the ids to place at this position; splits found inside go to `today_extra`.
    fn claim_pinned(&mut self, id: Id, depth: usize, today_extra: &mut Vec<Id>) -> Vec<Id> {
        match self.items.get(&id) {
            Some(Item::Tab(_)) => self.claim_tab(id).into_iter().collect(),
            Some(Item::Split(_)) => {
                self.warnings.push(format!("split {id} outside Today moved to Today"));
                today_extra.extend(self.claim_split(id));
                Vec::new()
            }
            Some(Item::Folder(_)) => {
                let Some(Item::Folder(mut f)) = self.take(id) else { return Vec::new() };
                // Claim first so a folder listing itself can't recurse.
                self.claimed.insert(id, Item::Folder(Folder { children: Vec::new(), ..f.clone() }));
                let mut children = Vec::new();
                for c in std::mem::take(&mut f.children) {
                    children.extend(self.claim_pinned(c, depth + 1, today_extra));
                }
                if depth > MAX_FOLDER_DEPTH {
                    self.warnings.push(format!("folder {id} nested too deep; flattened"));
                    self.claimed.remove(&id);
                    return children;
                }
                f.children = children;
                self.claimed.insert(id, Item::Folder(f));
                vec![id]
            }
            None => {
                self.take(id);
                Vec::new()
            }
        }
    }
}

/// Visit every persisted id field (not the `items` map keys, which equal the items' own ids).
fn for_each_id(state: &mut State, f: &mut dyn FnMut(&mut Id)) {
    for s in &mut state.spaces {
        f(&mut s.id);
        s.pinned.iter_mut().chain(s.today.iter_mut()).chain(s.active_item.as_mut()).for_each(&mut *f);
    }
    state.favorites.iter_mut().for_each(&mut *f);
    for item in state.items.values_mut() {
        match item {
            Item::Tab(t) => {
                f(&mut t.id);
                t.opener.iter_mut().for_each(&mut *f);
            }
            Item::Folder(folder) => {
                f(&mut folder.id);
                folder.children.iter_mut().for_each(&mut *f);
            }
            Item::Split(s) => {
                f(&mut s.id);
                s.panes.iter_mut().for_each(&mut *f);
            }
        }
    }
    for e in &mut state.archive {
        f(&mut e.id);
        e.space.iter_mut().chain(e.folder.iter_mut()).for_each(&mut *f);
        if let Some(snap) = &mut e.split {
            f(&mut snap.group);
            snap.panes.iter_mut().for_each(&mut *f);
        }
    }
    state.boosts.iter_mut().for_each(|b| f(&mut b.id));
    f(&mut state.window.active_space);
    state.window.mru.iter_mut().for_each(&mut *f);
    for r in &mut state.reopen {
        match r {
            ReopenEntry::Archived { archive_id } => f(archive_id),
            ReopenEntry::Unloaded { tab, .. } => f(tab),
            ReopenEntry::Batch { archive_ids } | ReopenEntry::Split { archive_ids } => archive_ids.iter_mut().for_each(&mut *f),
        }
    }
}

/// When any id (or `nextId`) is above [`RENUMBER_ABOVE`], renumber every id to `1..=n` keeping
/// their order, rewriting every reference (containers, favorites, splits, openers, active items,
/// MRU, archive entries and their snapshots, boosts, the reopen stack), and restart `nextId` at
/// `n + 1`. The mapping is one-to-one over every id that appears anywhere, so all relations
/// (including dangling references, which the rest of `repair` drops) are preserved exactly.
fn renumber_ids(state: &mut State, report: &mut LoadReport) {
    let mut ids: BTreeSet<Id> = state.items.keys().copied().collect();
    for_each_id(state, &mut |id| {
        if *id != 0 {
            ids.insert(*id);
        }
    });
    let max = ids.last().copied().unwrap_or(0);
    if max <= RENUMBER_ABOVE {
        if state.next_id > RENUMBER_ABOVE {
            report.warnings.push(format!("nextId {} → {}", state.next_id, max + 1));
            state.next_id = max + 1;
        }
        return;
    }
    let map: BTreeMap<Id, Id> = ids.iter().copied().zip(1..).collect();
    for_each_id(state, &mut |id| {
        if let Some(new) = map.get(id) {
            *id = *new;
        }
    });
    state.items = std::mem::take(&mut state.items).into_iter().map(|(k, v)| (map.get(&k).copied().unwrap_or(k), v)).collect();
    state.next_id = map.len() as Id + 1;
    report.warnings.push(format!("ids up to {max} renumbered to 1..={}", map.len()));
}

fn repair(state: &mut State, now: Millis, report: &mut LoadReport) {
    renumber_ids(state, report);
    let mut warn = |s: String| report.warnings.push(s);

    // Settings.
    let hours = super::handlers::normalize_archive_hours(state.settings.archive_after_hours);
    if hours != state.settings.archive_after_hours {
        warn(format!("settings.archiveAfterHours {} → {hours}", state.settings.archive_after_hours));
        state.settings.archive_after_hours = hours;
    }
    if state.settings.download_dir.as_deref().is_some_and(|d| d.trim().is_empty()) {
        state.settings.download_dir = None;
    }

    // Ids: next_id above everything; unique space / boost ids (item ids are unique by key).
    let max_id = state
        .items
        .keys()
        .copied()
        .chain(state.spaces.iter().map(|s| s.id))
        .chain(state.boosts.iter().map(|b| b.id))
        .chain(state.archive.iter().map(|e| e.id))
        .chain(state.archive.iter().filter_map(|e| e.split.as_ref().map(|s| s.group)))
        .filter(|id| valid_id(*id))
        .max()
        .unwrap_or(0);
    if state.next_id <= max_id {
        warn(format!("nextId {} → {}", state.next_id, max_id + 1));
        state.next_id = max_id + 1;
    }
    let mut used: BTreeSet<Id> = state.items.keys().copied().collect();
    for s in &mut state.spaces {
        if !valid_id(s.id) || used.contains(&s.id) {
            let new = state.next_id;
            state.next_id += 1;
            warn(format!("space id {} reassigned to {new}", s.id));
            if state.window.active_space == s.id && valid_id(s.id) {
                state.window.active_space = new;
            }
            s.id = new;
        }
        used.insert(s.id);
        let theme = crate::theme::sanitize_theme(&s.theme);
        if theme != s.theme {
            warn(format!("space {} theme clamped", s.id));
            s.theme = theme;
        }
        if s.name.trim().is_empty() {
            s.name = "Space".into();
        }
        if s.icon.trim().is_empty() {
            s.icon = "✨".into();
        }
    }
    for b in &mut state.boosts {
        if !valid_id(b.id) || used.contains(&b.id) {
            let new = state.next_id;
            state.next_id += 1;
            warn(format!("boost id {} reassigned to {new}", b.id));
            b.id = new;
        }
        used.insert(b.id);
    }

    // At least one space.
    if state.spaces.is_empty() {
        let id = state.next_id;
        state.next_id += 1;
        warn("no spaces; created Home".into());
        state.spaces.push(Space { id, name: "Home".into(), icon: "🏠".into(), created_at: now, ..Space::default() });
    }
    if !state.spaces.iter().any(|s| s.id == state.window.active_space) {
        state.window.active_space = state.spaces[0].id;
    }

    // Structure: claim every item for exactly one container.
    let mut c = Claimer { items: std::mem::take(&mut state.items), claimed: BTreeMap::new(), warnings: Vec::new() };
    let mut favorites = Vec::new();
    let mut overflow = Vec::new();
    for id in std::mem::take(&mut state.favorites) {
        if favorites.len() >= MAX_FAVORITES {
            if matches!(c.items.get(&id), Some(Item::Tab(_))) {
                overflow.extend(c.claim_tab(id));
                c.warnings.push(format!("favorites over {MAX_FAVORITES}: {id} moved to Today"));
            }
            continue;
        }
        if matches!(c.items.get(&id), Some(Item::Tab(_))) {
            favorites.extend(c.claim_tab(id));
        } else if c.items.contains_key(&id) {
            c.warnings.push(format!("favorite {id} is not a tab; ignored"));
        } else {
            c.take(id);
        }
    }
    let active = state.window.active_space;
    for space in &mut state.spaces {
        let mut today_extra = Vec::new();
        let mut pinned = Vec::new();
        let mut folders_from_today = Vec::new();
        for id in std::mem::take(&mut space.pinned) {
            pinned.extend(c.claim_pinned(id, 1, &mut today_extra));
        }
        let mut today = Vec::new();
        for id in std::mem::take(&mut space.today) {
            match c.items.get(&id) {
                Some(Item::Tab(_)) => today.extend(c.claim_tab(id)),
                Some(Item::Split(_)) => today.extend(c.claim_split(id)),
                Some(Item::Folder(_)) => {
                    c.warnings.push(format!("folder {id} in Today moved to Pinned"));
                    folders_from_today.extend(c.claim_pinned(id, 1, &mut today_extra));
                }
                None => {
                    c.take(id);
                }
            }
        }
        pinned.extend(folders_from_today);
        today_extra.extend(today);
        if space.id == active {
            today_extra.splice(0..0, overflow.drain(..));
        }
        space.pinned = pinned;
        space.today = today_extra;
    }
    if !c.items.is_empty() {
        let ids: Vec<String> = c.items.keys().map(|k| k.to_string()).collect();
        c.warnings.push(format!("dropped {} orphan item(s): {}", ids.len(), ids.join(", ")));
    }
    report.warnings.append(&mut c.warnings);
    state.items = c.claimed;
    state.favorites = favorites;

    // Tab invariants per section; pinned tabs reopen at their pinned URL.
    let pinned_tabs: BTreeSet<Id> = {
        let mut set: BTreeSet<Id> = state.favorites.iter().copied().collect();
        let mut stack: Vec<Id> = state.spaces.iter().flat_map(|s| s.pinned.iter().copied()).collect();
        while let Some(id) = stack.pop() {
            match state.items.get(&id) {
                Some(Item::Tab(_)) => {
                    set.insert(id);
                }
                Some(Item::Folder(f)) => stack.extend(f.children.iter().copied()),
                _ => {}
            }
        }
        set
    };
    for item in state.items.values_mut() {
        if let Item::Tab(t) = item {
            if pinned_tabs.contains(&t.id) {
                let pinned = match t.pinned_url.as_deref().map(str::trim) {
                    Some(p) if !p.is_empty() => p.to_string(),
                    _ => {
                        report.warnings.push(format!("pinned tab {} had no pinnedUrl", t.id));
                        t.url.clone()
                    }
                };
                t.url = pinned.clone();
                t.pinned_url = Some(pinned);
            } else if t.pinned_url.take().is_some() {
                report.warnings.push(format!("Today tab {} had a pinnedUrl; cleared", t.id));
            }
            if t.opener == Some(t.id) {
                t.opener = None;
            }
        }
    }
    // Openers of removed tabs (second pass: all items known now).
    let tab_ids: BTreeSet<Id> = state.items.iter().filter(|(_, i)| matches!(i, Item::Tab(_))).map(|(k, _)| *k).collect();
    for item in state.items.values_mut() {
        if let Item::Tab(t) = item
            && t.opener.is_some_and(|o| !tab_ids.contains(&o))
        {
            t.opener = None;
        }
    }

    // Active items: must be a tab/split owned by the space or a favorite (a pane → its split).
    let mut owner: BTreeMap<Id, Option<Id>> = BTreeMap::new(); // top-level id → space (None = favorite)
    let mut pane_of: BTreeMap<Id, Id> = BTreeMap::new();
    for f in &state.favorites {
        owner.insert(*f, None);
    }
    for s in &state.spaces {
        let mut stack: Vec<Id> = s.pinned.iter().chain(s.today.iter()).copied().collect();
        while let Some(id) = stack.pop() {
            match state.items.get(&id) {
                Some(Item::Folder(f)) => stack.extend(f.children.iter().copied()),
                Some(Item::Split(sp)) => {
                    owner.insert(id, Some(s.id));
                    for p in &sp.panes {
                        pane_of.insert(*p, id);
                    }
                }
                Some(Item::Tab(_)) => {
                    owner.insert(id, Some(s.id));
                }
                None => {}
            }
        }
    }
    for s in &mut state.spaces {
        if let Some(a) = s.active_item {
            let a = pane_of.get(&a).copied().unwrap_or(a);
            let valid = match owner.get(&a) {
                Some(None) => true,
                Some(Some(sid)) => *sid == s.id,
                None => false,
            };
            if valid {
                s.active_item = Some(a);
            } else {
                report.warnings.push(format!("space {}: active item {a} invalid; cleared", s.id));
                s.active_item = None;
            }
        }
    }

    // Window.
    let w = &mut state.window;
    let clamped = w.sidebar_width.clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH);
    if clamped != w.sidebar_width {
        report.warnings.push(format!("sidebarWidth {} → {clamped}", w.sidebar_width));
        w.sidebar_width = clamped;
    }
    let mut seen = BTreeSet::new();
    w.mru.retain(|t| tab_ids.contains(t) && seen.insert(*t));
    w.mru.truncate(MAX_MRU);
    if let Some(b) = w.bounds
        && (b.width <= 0 || b.height <= 0)
    {
        w.bounds = None;
    }

    // Archive: valid unique ids, newest first, sane snapshots. An out-of-range split group id
    // gets one fresh id per group, so the group still restores together.
    let mut seen = BTreeSet::new();
    let before = state.archive.len();
    state.archive.retain(|e| valid_id(e.id) && seen.insert(e.id));
    if state.archive.len() != before {
        report.warnings.push(format!("archive: {} entries with invalid or duplicate ids dropped", before - state.archive.len()));
    }
    let mut regrouped: BTreeMap<Id, Id> = BTreeMap::new();
    for e in &mut state.archive {
        if let Some(snap) = &mut e.split {
            if snap.fractions.iter().any(|f| !f.is_finite()) {
                snap.fractions.clear();
            }
            snap.panes.retain(|p| valid_id(*p));
            if !valid_id(snap.group) {
                let fresh = *regrouped.entry(snap.group).or_insert_with(|| {
                    let id = state.next_id;
                    state.next_id += 1;
                    id
                });
                report.warnings.push(format!("archive entry {}: split group {} reassigned to {fresh}", e.id, snap.group));
                snap.group = fresh;
            }
        }
    }

    // Timestamps: 0..=MAX_MILLIS, so time arithmetic can't overflow.
    let mut clamped = 0;
    for item in state.items.values_mut() {
        if let Item::Tab(t) = item {
            clamp_time(&mut t.created_at, &mut clamped);
            clamp_time(&mut t.last_active_at, &mut clamped);
        }
    }
    for s in &mut state.spaces {
        clamp_time(&mut s.created_at, &mut clamped);
    }
    for e in &mut state.archive {
        clamp_time(&mut e.archived_at, &mut clamped);
    }
    for b in &mut state.boosts {
        clamp_time(&mut b.created_at, &mut clamped);
        clamp_time(&mut b.updated_at, &mut clamped);
    }
    if clamped > 0 {
        report.warnings.push(format!("{clamped} out-of-range timestamp(s) clamped"));
    }
    state.archive.sort_by_key(|e| std::cmp::Reverse(e.archived_at));

    // Reopen stack: drop entries whose references are gone; cap.
    let archive_ids: BTreeSet<Id> = state.archive.iter().map(|e| e.id).collect();
    let before = state.reopen.len();
    state.reopen.retain(|r| match r {
        ReopenEntry::Archived { archive_id } => archive_ids.contains(archive_id),
        ReopenEntry::Unloaded { tab, .. } => pinned_tabs.contains(tab) && tab_ids.contains(tab),
        ReopenEntry::Batch { archive_ids: ids } | ReopenEntry::Split { archive_ids: ids } => ids.iter().any(|i| archive_ids.contains(i)),
    });
    if state.reopen.len() > MAX_REOPEN_STACK {
        let excess = state.reopen.len() - MAX_REOPEN_STACK;
        state.reopen.drain(..excess);
    }
    if state.reopen.len() != before {
        report.warnings.push(format!("reopen stack: {} stale entries dropped", before - state.reopen.len()));
    }

    // Site permissions: one decision per (origin, kind), last wins.
    let mut perms: Vec<SitePermission> = Vec::new();
    for p in std::mem::take(&mut state.site_permissions) {
        let origin = crate::urls::normalize_origin(&p.origin);
        perms.retain(|q| !(q.origin == origin && q.kind == p.kind));
        perms.push(SitePermission { origin, ..p });
    }
    state.site_permissions = perms;
    state.version = STATE_VERSION;
}

fn parse_history(text: &str, now: Millis, report: &mut LoadReport, repaired: &mut bool) -> History {
    let obj = match serde_json::from_str::<Value>(text) {
        Ok(mut root @ Value::Object(_)) => {
            if upgrade_legacy_urls(&mut root, "history", report) {
                *repaired = true;
            }
            let Value::Object(o) = root else { unreachable!("matched an object") };
            o
        }
        Ok(_) | Err(_) => {
            report.history_corrupt = true;
            report.warnings.push("history.json is invalid; starting with empty history".into());
            return History::default();
        }
    };
    let mut history = History::default();
    let urls = match obj.get("urls") {
        Some(Value::Array(a)) => a.as_slice(),
        None | Some(Value::Null) => &[],
        Some(_) => {
            report.history_corrupt = true;
            report.warnings.push("history.urls is not an array".into());
            &[]
        }
    };
    let mut seen = BTreeSet::new();
    let mut dropped = 0usize;
    let mut clamped = 0usize;
    for v in urls {
        let mut sub = LoadReport::default();
        let Some(mut u) = tolerant::<HistoryUrl>(v, "history", &mut sub) else {
            dropped += 1;
            continue;
        };
        if sub.state_corrupt {
            report.history_corrupt = true;
        }
        if u.url.trim().is_empty() || !seen.insert(u.url.clone()) {
            continue;
        }
        if u.visits.len() > MAX_VISITS_PER_URL {
            let excess = u.visits.len() - MAX_VISITS_PER_URL;
            u.visits.drain(..excess);
        }
        clamp_time(&mut u.last_visit_at, &mut clamped);
        for visit in &mut u.visits {
            clamp_time(&mut visit.at, &mut clamped);
        }
        u.visit_count = u.visit_count.max(u.visits.len() as u32);
        u.frecency = crate::history::frecency(&u, now);
        history.urls.push(u);
    }
    if dropped > 0 {
        report.history_corrupt = true;
        report.warnings.push(format!("history: {dropped} invalid entries dropped"));
    }
    if clamped > 0 {
        *repaired = true;
        report.warnings.push(format!("history: {clamped} out-of-range timestamp(s) clamped"));
    }
    if history.urls.len() > MAX_HISTORY_URLS {
        history.evict();
    }
    history
}
