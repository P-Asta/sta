//! Element refs (docs/MCP.md "Refs"): `page_snapshot` hands out refs like `42.3.17` — tab `42`,
//! document generation `3`, element `17`. The shell maps each ref to the element's DevTools
//! backend node id and the document (loader id) it was taken from. A new main-frame document, an
//! agent detach or a renderer crash starts a new generation, so old refs fail with `stale_ref`
//! instead of hitting a different element (backend node ids are reused across documents, see
//! docs/research/automation.md). Refs survive a bridge restart because they carry the tab.

use crate::Id;
use std::collections::{HashMap, VecDeque};
use std::fmt;

/// Most refs kept per tab (least recently used are dropped first).
pub const MAX_REFS_PER_TAB: usize = 5_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RefId {
    pub tab: Id,
    pub generation: u64,
    pub n: u32,
}

impl RefId {
    /// Parses `tab.generation.n` (decimal, no signs or spaces).
    pub fn parse(text: &str) -> Option<RefId> {
        let mut parts = text.trim().split('.');
        let num = |p: Option<&str>| p.filter(|s| !s.is_empty() && s.len() <= 16 && s.bytes().all(|b| b.is_ascii_digit())).and_then(|s| s.parse::<u64>().ok());
        let tab = num(parts.next())?;
        let generation = num(parts.next())?;
        let n = u32::try_from(num(parts.next())?).ok()?;
        if parts.next().is_some() || tab == 0 || tab > crate::MAX_ID {
            return None;
        }
        Some(RefId { tab, generation, n })
    }
}

impl fmt::Display for RefId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.tab, self.generation, self.n)
    }
}

/// Why a ref can't be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefProblem {
    /// Not a ref at all.
    Malformed,
    /// Belongs to another tab than the one the call targets.
    WrongTab,
    /// From an older document (or evicted).
    Stale,
}

/// The refs of one tab. `T` is the shell's element handle (backend node id, loader id, frame).
#[derive(Debug, Clone)]
pub struct RefTable<T> {
    tab: Id,
    generation: u64,
    next: u32,
    by_n: HashMap<u32, (i64, T)>,
    by_backend: HashMap<i64, u32>,
    /// Least recently used first.
    lru: VecDeque<u32>,
}

impl<T: Clone> RefTable<T> {
    pub fn new(tab: Id) -> Self {
        Self { tab, generation: 1, next: 1, by_n: HashMap::new(), by_backend: HashMap::new(), lru: VecDeque::new() }
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// A new document: every ref handed out so far is stale.
    pub fn invalidate(&mut self) {
        self.generation += 1;
        self.next = 1;
        self.by_n.clear();
        self.by_backend.clear();
        self.lru.clear();
    }

    fn touch(&mut self, n: u32) {
        if let Some(pos) = self.lru.iter().position(|x| *x == n) {
            self.lru.remove(pos);
        }
        self.lru.push_back(n);
    }

    /// The ref of an element (the same one again for an element seen before in this generation).
    pub fn ref_for(&mut self, backend_node_id: i64, handle: T) -> RefId {
        if let Some(n) = self.by_backend.get(&backend_node_id).copied() {
            if let Some(entry) = self.by_n.get_mut(&n) {
                entry.1 = handle;
            }
            self.touch(n);
            return RefId { tab: self.tab, generation: self.generation, n };
        }
        let n = self.next;
        self.next = self.next.saturating_add(1);
        self.by_n.insert(n, (backend_node_id, handle));
        self.by_backend.insert(backend_node_id, n);
        self.lru.push_back(n);
        while self.lru.len() > MAX_REFS_PER_TAB {
            if let Some(old) = self.lru.pop_front()
                && let Some((backend, _)) = self.by_n.remove(&old)
            {
                self.by_backend.remove(&backend);
            }
        }
        RefId { tab: self.tab, generation: self.generation, n }
    }

    /// Looks a ref up: `(backend node id, handle)`.
    pub fn resolve(&mut self, text: &str) -> Result<(i64, T), RefProblem> {
        let id = RefId::parse(text).ok_or(RefProblem::Malformed)?;
        if id.tab != self.tab {
            return Err(RefProblem::WrongTab);
        }
        if id.generation != self.generation {
            return Err(RefProblem::Stale);
        }
        let found = self.by_n.get(&id.n).cloned().ok_or(RefProblem::Stale)?;
        self.touch(id.n);
        Ok(found)
    }

    pub fn len(&self) -> usize {
        self.by_n.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_n.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_format() {
        let r = RefId { tab: 42, generation: 3, n: 17 };
        assert_eq!(r.to_string(), "42.3.17");
        assert_eq!(RefId::parse(" 42.3.17 "), Some(r));
        for bad in ["", "42", "42.3", "42.3.17.1", "a.3.17", "-1.3.17", "42.3.+17", "0.1.1", "42.3.99999999999", "42..17"] {
            assert_eq!(RefId::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn stable_within_a_generation_and_stale_after() {
        let mut t: RefTable<&str> = RefTable::new(7);
        let a = t.ref_for(100, "doc1");
        let b = t.ref_for(200, "doc1");
        assert_ne!(a, b);
        assert_eq!(t.ref_for(100, "doc1"), a, "same element, same ref");
        assert_eq!(t.resolve(&a.to_string()), Ok((100, "doc1")));
        assert_eq!(t.resolve("8.1.1"), Err(RefProblem::WrongTab));
        assert_eq!(t.resolve("7.1.99"), Err(RefProblem::Stale));
        assert_eq!(t.resolve("nope"), Err(RefProblem::Malformed));
        t.invalidate();
        assert_eq!(t.resolve(&a.to_string()), Err(RefProblem::Stale));
        // Backend ids are reused by the next document: the new ref has a new generation.
        let c = t.ref_for(100, "doc2");
        assert_eq!(c.generation, 2);
        assert_eq!(t.resolve(&c.to_string()), Ok((100, "doc2")));
    }

    #[test]
    fn lru_cap() {
        let mut t: RefTable<()> = RefTable::new(1);
        let first = t.ref_for(0, ());
        for i in 1..=MAX_REFS_PER_TAB as i64 {
            t.ref_for(i, ());
        }
        assert_eq!(t.len(), MAX_REFS_PER_TAB);
        assert_eq!(t.resolve(&first.to_string()), Err(RefProblem::Stale), "oldest evicted");
    }
}
