//! Structural queries and edits on the sidebar tree (containers, parents, visual order).

use super::*;

/// The container an item id is listed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Parent {
    Favorites,
    Pinned(Id),
    Today(Id),
    Folder(Id),
    Split(Id),
}

impl Store {
    pub(super) fn tab_item(&self, id: Id) -> Option<&Tab> {
        match self.state.items.get(&id) {
            Some(Item::Tab(t)) => Some(t),
            _ => None,
        }
    }

    pub(super) fn tab_item_mut(&mut self, id: Id) -> Option<&mut Tab> {
        match self.state.items.get_mut(&id) {
            Some(Item::Tab(t)) => Some(t),
            _ => None,
        }
    }

    pub(super) fn split_item(&self, id: Id) -> Option<&Split> {
        match self.state.items.get(&id) {
            Some(Item::Split(s)) => Some(s),
            _ => None,
        }
    }

    pub(super) fn split_mut(&mut self, id: Id) -> Option<&mut Split> {
        match self.state.items.get_mut(&id) {
            Some(Item::Split(s)) => Some(s),
            _ => None,
        }
    }

    pub(super) fn folder_item(&self, id: Id) -> Option<&Folder> {
        match self.state.items.get(&id) {
            Some(Item::Folder(f)) => Some(f),
            _ => None,
        }
    }

    pub(super) fn folder_mut(&mut self, id: Id) -> Option<&mut Folder> {
        match self.state.items.get_mut(&id) {
            Some(Item::Folder(f)) => Some(f),
            _ => None,
        }
    }

    pub(super) fn space(&self, id: Id) -> Option<&Space> {
        self.state.spaces.iter().find(|s| s.id == id)
    }

    pub(super) fn space_mut(&mut self, id: Id) -> Option<&mut Space> {
        self.state.spaces.iter_mut().find(|s| s.id == id)
    }

    pub(super) fn active_space_id(&self) -> Id {
        self.state.window.active_space
    }

    /// Active item (tab or split) of the active space.
    pub(super) fn active_item(&self) -> Option<Id> {
        self.space(self.state.window.active_space).and_then(|s| s.active_item)
    }

    /// Focused tab of an item: the tab itself, or the focused pane of a split.
    pub(super) fn focused_of_item(&self, item: Id) -> Option<Id> {
        match self.state.items.get(&item)? {
            Item::Tab(t) => Some(t.id),
            Item::Split(s) => s.panes.get(s.focused).or(s.panes.first()).copied(),
            Item::Folder(_) => None,
        }
    }

    /// Focused tab of the content area (ignores Peek).
    pub(super) fn content_focused_tab(&self) -> Option<Id> {
        self.active_item().and_then(|i| self.focused_of_item(i))
    }

    /// Tabs shown in the content area for the active item (all panes of a split).
    pub(super) fn layout_tab_ids(&self) -> Vec<Id> {
        match self.active_item().and_then(|i| self.state.items.get(&i)) {
            Some(Item::Tab(t)) => vec![t.id],
            Some(Item::Split(s)) => s.panes.clone(),
            _ => Vec::new(),
        }
    }

    /// The content layout for the active item, excluding tabs whose creation is deferred.
    pub(super) fn desired_layout(&self) -> ContentLayout {
        match self.active_item().and_then(|i| self.state.items.get(&i)) {
            Some(Item::Tab(t)) if !self.rt.pending_create.contains_key(&t.id) => ContentLayout::Single { tab: t.id },
            Some(Item::Split(s)) => {
                let focused_tab = s.panes.get(s.focused).copied();
                let kept: Vec<(Id, f32)> = s
                    .panes
                    .iter()
                    .zip(s.fractions.iter().chain(std::iter::repeat(&0.0)))
                    .filter(|(t, _)| !self.rt.pending_create.contains_key(t))
                    .map(|(t, f)| (*t, *f))
                    .collect();
                match kept.len() {
                    0 => ContentLayout::Empty,
                    1 => ContentLayout::Single { tab: kept[0].0 },
                    _ => {
                        let mut fractions: Vec<f32> = kept.iter().map(|k| k.1).collect();
                        normalize_fractions(&mut fractions, 0.0);
                        let focused = kept.iter().position(|k| Some(k.0) == focused_tab).unwrap_or(0);
                        ContentLayout::Split {
                            orientation: s.orientation,
                            panes: kept.iter().zip(fractions).map(|(k, fraction)| Pane { tab: k.0, fraction }).collect(),
                            focused,
                        }
                    }
                }
            }
            _ => ContentLayout::Empty,
        }
    }

    /// Where an item id is listed, with its index.
    pub(super) fn parent_of(&self, id: Id) -> Option<(Parent, usize)> {
        if let Some(i) = self.state.favorites.iter().position(|x| *x == id) {
            return Some((Parent::Favorites, i));
        }
        for s in &self.state.spaces {
            if let Some(i) = s.pinned.iter().position(|x| *x == id) {
                return Some((Parent::Pinned(s.id), i));
            }
            if let Some(i) = s.today.iter().position(|x| *x == id) {
                return Some((Parent::Today(s.id), i));
            }
        }
        for item in self.state.items.values() {
            match item {
                Item::Folder(f) => {
                    if let Some(i) = f.children.iter().position(|x| *x == id) {
                        return Some((Parent::Folder(f.id), i));
                    }
                }
                Item::Split(s) => {
                    if let Some(i) = s.panes.iter().position(|x| *x == id) {
                        return Some((Parent::Split(s.id), i));
                    }
                }
                Item::Tab(_) => {}
            }
        }
        None
    }

    pub(super) fn container(&self, p: Parent) -> Option<&Vec<Id>> {
        match p {
            Parent::Favorites => Some(&self.state.favorites),
            Parent::Pinned(s) => self.space(s).map(|s| &s.pinned),
            Parent::Today(s) => self.space(s).map(|s| &s.today),
            Parent::Folder(f) => self.folder_item(f).map(|f| &f.children),
            Parent::Split(s) => self.split_item(s).map(|s| &s.panes),
        }
    }

    fn container_mut(&mut self, p: Parent) -> Option<&mut Vec<Id>> {
        match p {
            Parent::Favorites => Some(&mut self.state.favorites),
            Parent::Pinned(s) => self.space_mut(s).map(|s| &mut s.pinned),
            Parent::Today(s) => self.space_mut(s).map(|s| &mut s.today),
            Parent::Folder(f) => self.folder_mut(f).map(|f| &mut f.children),
            Parent::Split(s) => self.split_mut(s).map(|s| &mut s.panes),
        }
    }

    /// Space owning an item (walking up folders and splits); `None` for favorites / unknown.
    pub(super) fn space_of(&self, id: Id) -> Option<Id> {
        let mut cur = id;
        for _ in 0..64 {
            match self.parent_of(cur)?.0 {
                Parent::Favorites => return None,
                Parent::Pinned(s) | Parent::Today(s) => return Some(s),
                Parent::Folder(f) => cur = f,
                Parent::Split(s) => cur = s,
            }
        }
        None
    }

    /// Section of any item (panes → Today).
    pub(super) fn section_of(&self, id: Id) -> Option<Section> {
        let mut cur = id;
        for _ in 0..64 {
            match self.parent_of(cur)?.0 {
                Parent::Favorites => return Some(Section::Favorites),
                Parent::Pinned(_) => return Some(Section::Pinned),
                Parent::Today(_) => return Some(Section::Today),
                Parent::Folder(f) => cur = f,
                Parent::Split(s) => cur = s,
            }
        }
        None
    }

    /// The sidebar row an id belongs to: a pane's split, otherwise the id itself.
    pub(super) fn top_level_of(&self, id: Id) -> Id {
        match self.parent_of(id) {
            Some((Parent::Split(s), _)) => s,
            _ => id,
        }
    }

    /// Depth of a folder: 1 for a top-level pinned folder.
    pub(super) fn folder_depth(&self, id: Id) -> usize {
        let mut depth = 1;
        let mut cur = id;
        while let Some((Parent::Folder(p), _)) = self.parent_of(cur) {
            depth += 1;
            cur = p;
            if depth > 64 {
                break;
            }
        }
        depth
    }

    /// Height of a folder subtree: 1 for a folder without subfolders.
    pub(super) fn folder_height(&self, id: Id) -> usize {
        fn h(store: &Store, id: Id, guard: usize) -> usize {
            if guard > 64 {
                return 1;
            }
            let Some(f) = store.folder_item(id) else { return 0 };
            1 + f.children.iter().map(|c| h(store, *c, guard + 1)).max().unwrap_or(0)
        }
        h(self, id, 0)
    }

    /// Is `candidate` inside folder `folder` (at any depth)?
    pub(super) fn is_descendant(&self, folder: Id, candidate: Id) -> bool {
        let mut cur = candidate;
        for _ in 0..64 {
            match self.parent_of(cur) {
                Some((Parent::Folder(p), _)) => {
                    if p == folder {
                        return true;
                    }
                    cur = p;
                }
                _ => return false,
            }
        }
        false
    }

    /// Every tab under an item (the tab itself, split panes, folder contents recursively).
    pub(super) fn tabs_under(&self, id: Id) -> Vec<Id> {
        let mut out = Vec::new();
        let mut stack = vec![id];
        let mut guard = 0;
        while let Some(cur) = stack.pop() {
            guard += 1;
            if guard > 100_000 {
                break;
            }
            match self.state.items.get(&cur) {
                Some(Item::Tab(t)) => out.push(t.id),
                Some(Item::Split(s)) => out.extend(s.panes.iter().copied()),
                Some(Item::Folder(f)) => stack.extend(f.children.iter().rev().copied()),
                None => {}
            }
        }
        out
    }

    /// May `item` be the active item of `space`? (A tab or split owned by the space, or a
    /// favorite tab; never a pane or folder.)
    pub(super) fn valid_active_for(&self, space: Id, item: Id) -> bool {
        match self.state.items.get(&item) {
            Some(Item::Tab(_)) | Some(Item::Split(_)) => {}
            _ => return false,
        }
        match self.parent_of(item) {
            Some((Parent::Favorites, _)) => matches!(self.state.items.get(&item), Some(Item::Tab(_))),
            Some((Parent::Split(_), _)) | None => false,
            Some(_) => self.space_of(item) == Some(space),
        }
    }

    /// Activatable rows in visual order: favorites, visible pinned rows (depth-first, skipping
    /// collapsed folders' children; folder rows themselves are skipped), Today items.
    pub(super) fn visual_order(&self) -> Vec<Id> {
        let mut out: Vec<Id> = self.state.favorites.clone();
        if let Some(space) = self.space(self.state.window.active_space) {
            fn walk(store: &Store, ids: &[Id], out: &mut Vec<Id>, depth: usize) {
                if depth > 64 {
                    return;
                }
                for id in ids {
                    match store.state.items.get(id) {
                        Some(Item::Tab(_)) => out.push(*id),
                        Some(Item::Folder(f)) if !f.collapsed => walk(store, &f.children, out, depth + 1),
                        _ => {}
                    }
                }
            }
            walk(self, &space.pinned, &mut out, 0);
            out.extend(space.today.iter().copied());
        }
        out
    }

    /// Remove an id from its container (the item itself stays in `items`). For a split pane
    /// the matching fraction is removed and `focused` adjusted (no renormalization, no
    /// dissolving).
    pub(super) fn unlink(&mut self, id: Id) -> Option<(Parent, usize)> {
        let (parent, idx) = self.parent_of(id)?;
        if let Parent::Split(sid) = parent {
            let s = self.split_mut(sid)?;
            s.panes.remove(idx);
            if idx < s.fractions.len() {
                s.fractions.remove(idx);
            }
            if s.focused > idx || (s.focused == idx && s.focused >= s.panes.len()) {
                s.focused = s.focused.saturating_sub(1);
            }
        } else {
            self.container_mut(parent)?.remove(idx);
        }
        self.dirty.state = true;
        Some((parent, idx))
    }

    /// Insert an id into a container at `index` (clamped).
    pub(super) fn insert_into(&mut self, parent: Parent, index: usize, id: Id) -> bool {
        let Some(c) = self.container_mut(parent) else { return false };
        let i = index.min(c.len());
        c.insert(i, id);
        if let Parent::Split(sid) = parent
            && let Some(s) = self.split_mut(sid)
        {
            s.fractions.insert(i.min(s.fractions.len()), 0.0);
            equalize(&mut s.fractions);
            if s.focused >= i && s.panes.len() > 1 && s.focused + 1 < s.panes.len() {
                s.focused += 1;
            }
        }
        self.dirty.state = true;
        true
    }

    /// A split left with fewer than two panes dissolves: one pane replaces the split in its
    /// container (and as active item); an empty split is removed. Returns the remaining tab.
    pub(super) fn dissolve_if_needed(&mut self, sid: Id) -> Option<Id> {
        let s = self.split_item(sid)?;
        if s.panes.len() >= 2 {
            let mut f = s.fractions.clone();
            normalize_fractions(&mut f, 0.0);
            if let Some(s) = self.split_mut(sid) {
                s.fractions = f;
            }
            return None;
        }
        let remaining = s.panes.first().copied();
        let (parent, idx) = self.parent_of(sid)?;
        if let Some(c) = self.container_mut(parent) {
            match remaining {
                Some(t) => c[idx] = t,
                None => {
                    c.remove(idx);
                }
            }
        }
        self.state.items.remove(&sid);
        for space in &mut self.state.spaces {
            if space.active_item == Some(sid) {
                space.active_item = remaining;
            }
        }
        self.touch();
        remaining
    }

    /// Repair `Space::active_item`s that point at removed / moved items. The active space falls
    /// back to its most recent MRU item (and loads it); other spaces just record the fallback.
    pub(super) fn repair_active_items(&mut self, _now: Millis) {
        let active = self.state.window.active_space;
        if self.space(active).is_none()
            && let Some(first) = self.state.spaces.first().map(|s| s.id)
        {
            self.state.window.active_space = first;
            self.touch();
        }
        let active = self.state.window.active_space;
        let ids: Vec<(Id, Option<Id>)> = self.state.spaces.iter().map(|s| (s.id, s.active_item)).collect();
        for (sid, item) in ids {
            let Some(item) = item else { continue };
            if self.valid_active_for(sid, item) {
                continue;
            }
            // A pane recorded as active item → its split.
            let top = self.top_level_of(item);
            let replacement = if top != item && self.valid_active_for(sid, top) {
                Some(top)
            } else {
                self.fallback_item(sid, &[item], None).map(|c| self.top_level_of(c))
            };
            if let Some(space) = self.space_mut(sid) {
                space.active_item = replacement;
            }
            if sid == active {
                self.rt.focus_request = replacement.and_then(|r| self.focused_of_item(r));
            }
            self.touch();
        }
    }

    /// Candidate to activate in `space` after its active item went away: `opener` (if still a
    /// valid item for the space), else the most recent MRU tab of the space (favorites count).
    /// Returns a tab id (a pane id activates its split with that pane focused) or split id.
    pub(super) fn fallback_item(&self, space: Id, exclude: &[Id], opener: Option<Id>) -> Option<Id> {
        let ok = |t: Id| {
            let top = self.top_level_of(t);
            !exclude.contains(&t) && !exclude.contains(&top) && self.valid_active_for(space, top)
        };
        if let Some(o) = opener.filter(|o| self.tab_item(*o).is_some())
            && ok(o)
        {
            return Some(o);
        }
        self.state.window.mru.iter().copied().find(|t| self.tab_item(*t).is_some() && ok(*t))
    }
}

/// Equal fractions summing to 1.
pub(super) fn equalize(f: &mut [f32]) {
    let n = f.len();
    if n > 0 {
        for v in f.iter_mut() {
            *v = 1.0 / n as f32;
        }
    }
}

/// Renormalize fractions to sum 1 with each at least `min` (capped at 1/n). Non-finite or
/// non-positive values count as `min` (or an equal share when `min` is 0).
pub(super) fn normalize_fractions(f: &mut [f32], min: f32) {
    let n = f.len();
    if n == 0 {
        return;
    }
    let min = min.clamp(0.0, 1.0 / n as f32);
    let fill = if min > 0.0 { min } else { 1.0 / n as f32 };
    for v in f.iter_mut() {
        if !v.is_finite() || *v <= 0.0 {
            *v = fill;
        }
    }
    let sum: f32 = f.iter().sum();
    if !(sum.is_finite() && sum > 0.0) {
        equalize(f);
        return;
    }
    for v in f.iter_mut() {
        *v /= sum;
    }
    if min <= 0.0 {
        return;
    }
    let mut fixed = vec![false; n];
    for _ in 0..n {
        let low: Vec<usize> = (0..n).filter(|i| !fixed[*i] && f[*i] < min).collect();
        if low.is_empty() {
            break;
        }
        for i in low {
            fixed[i] = true;
            f[i] = min;
        }
        let fixed_sum: f32 = (0..n).filter(|i| fixed[*i]).map(|i| f[i]).sum();
        let free_sum: f32 = (0..n).filter(|i| !fixed[*i]).map(|i| f[i]).sum();
        if free_sum <= 0.0 {
            break;
        }
        let scale = (1.0 - fixed_sum) / free_sum;
        for i in 0..n {
            if !fixed[i] {
                f[i] *= scale;
            }
        }
    }
}
