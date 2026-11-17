use crate::{Error, Result};
use std::collections::{HashMap, HashSet};
use std::hash::Hash;

/// Maps refs (`e1`, `e2`, ...) to adapter handles, one map per session.
///
/// An element keeps its ref across snapshots while it stays on the page, so diffs and
/// agents can rely on it. Refs whose element disappeared become stale, never reused.
///
/// `H` must be unique across documents (e.g. include a document or loader id), because
/// some engines restart node ids on navigation.
#[derive(Debug)]
pub struct RefMap<H> {
    next: u32,
    by_handle: HashMap<H, String>,
    live: HashMap<String, H>,
    issued: HashSet<String>,
    pending: Option<HashMap<String, H>>,
}

impl<H: Clone + Eq + Hash> Default for RefMap<H> {
    fn default() -> Self {
        RefMap { next: 1, by_handle: HashMap::new(), live: HashMap::new(), issued: HashSet::new(), pending: None }
    }
}

impl<H: Clone + Eq + Hash> RefMap<H> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Numbering continues from another map, so its old refs can never name a new element.
    pub fn starting_at(next: u32) -> Self {
        RefMap { next, ..Self::default() }
    }

    /// The number the next new ref will get.
    pub fn next_index(&self) -> u32 {
        self.next
    }

    /// Start a new snapshot. Call `assign` for each element, then `finish`.
    pub fn begin(&mut self) {
        self.pending = Some(HashMap::new());
    }

    /// Ref for this element: the old one if it was seen before, else a new one.
    pub fn assign(&mut self, handle: H) -> String {
        let r = match self.by_handle.get(&handle) {
            Some(r) => r.clone(),
            None => {
                let r = format!("e{}", self.next);
                self.next += 1;
                self.by_handle.insert(handle.clone(), r.clone());
                self.issued.insert(r.clone());
                r
            }
        };
        self.pending.get_or_insert_with(HashMap::new).insert(r.clone(), handle);
        r
    }

    /// Make the refs assigned since `begin` the live set; everything else goes stale.
    pub fn finish(&mut self) {
        self.live = self.pending.take().unwrap_or_default();
        // ponytail: by_handle keeps gone handles so a returning element gets its old ref back;
        // bounded by elements seen per session, prune if sessions get very long.
    }

    /// Handle for a ref from the latest snapshot. Accepts "e3" or "@e3".
    pub fn resolve(&self, r: &str) -> Result<H> {
        let r = r.trim_start_matches('@');
        match self.live.get(r) {
            Some(h) => Ok(h.clone()),
            None if self.issued.contains(r) => Err(Error::StaleRef(r.into())),
            None => Err(Error::UnknownRef(r.into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(m: &mut RefMap<u32>, handles: &[u32]) -> Vec<String> {
        m.begin();
        let refs = handles.iter().map(|h| m.assign(*h)).collect();
        m.finish();
        refs
    }

    #[test]
    fn refs_are_stable_across_snapshots() {
        let mut m = RefMap::new();
        assert_eq!(snap(&mut m, &[10, 20]), ["e1", "e2"]);
        assert_eq!(snap(&mut m, &[20, 30, 10]), ["e2", "e3", "e1"]);
        assert_eq!(m.resolve("@e3").unwrap(), 30);
    }

    #[test]
    fn gone_element_is_stale_and_never_reused() {
        let mut m = RefMap::new();
        snap(&mut m, &[10, 20]);
        snap(&mut m, &[20, 40]);
        assert_eq!(m.resolve("e1"), Err(Error::StaleRef("e1".into())));
        assert_eq!(m.resolve("e3").unwrap(), 40);
    }

    #[test]
    fn starting_at_continues_numbering() {
        let mut m = RefMap::<u32>::starting_at(7);
        assert_eq!(snap(&mut m, &[1]), ["e7"]);
        assert_eq!(m.next_index(), 8);
        assert!(matches!(m.resolve("e1"), Err(Error::UnknownRef(_))));
    }

    #[test]
    fn never_issued_is_unknown() {
        let mut m = RefMap::<u32>::new();
        snap(&mut m, &[1]);
        assert_eq!(m.resolve("e9"), Err(Error::UnknownRef("e9".into())));
        assert!(m.resolve("e9").unwrap_err().to_string().contains("take a snapshot"));
    }

    #[test]
    fn refs_from_unfinished_snapshot_are_not_live() {
        let mut m = RefMap::new();
        snap(&mut m, &[1]);
        m.begin();
        m.assign(2);
        assert_eq!(m.resolve("e1").unwrap(), 1, "previous snapshot stays live until finish");
        assert!(m.resolve("e2").is_err());
    }
}
