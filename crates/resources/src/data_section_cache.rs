//! `DataSectionCache` port (`data_section_cache.cpp`): the LRU behind
//! `DirSection::findChild`, keyed on full resource paths.
//!
//! Reference quirks preserved:
//! - entries with zero bytes are never cached, and a section larger than the
//!   whole cache is never cached either (even into an empty cache);
//! - inserting purges least-recently-used entries until the newcomer fits;
//! - re-adding an existing path replaces the payload and byte count but
//!   keeps the entry's list position (no move-to-head);
//! - `find` moves hits to the head (most recent) and counts `hits`/`misses`;
//! - `clear` empties the chain *and resets the statistics*;
//! - the default size is 100 KiB (`DEFAULT_CACHE_SIZE`).

use std::collections::HashMap;
use std::sync::Arc;

/// `DEFAULT_CACHE_SIZE`: 100 KiB.
pub const DEFAULT_CACHE_SIZE: usize = 100 * 1024;

struct CacheNode<T> {
    bytes: usize,
    section: Arc<T>,
    /// newer -> older; the head is the most recently used entry.
    prev_to_newer: Option<String>,
    next_older: Option<String>,
}

/// `DataSectionCache`: LRU of sections keyed by full resource path.
pub struct DataSectionCache<T> {
    max_bytes: usize,
    current_bytes: usize,
    hits: usize,
    misses: usize,
    map: HashMap<String, CacheNode<T>>,
    head: Option<String>,
    tail: Option<String>,
}

impl<T> Default for DataSectionCache<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> DataSectionCache<T> {
    /// `DataSectionCache()` with `DEFAULT_CACHE_SIZE`.
    pub fn new() -> Self {
        Self::with_size(DEFAULT_CACHE_SIZE)
    }

    /// `setSize`: changes the budget. Existing entries are kept (the
    /// reference does not shrink on the spot either).
    pub fn set_size(&mut self, max_bytes: usize) {
        self.max_bytes = max_bytes;
    }

    /// Constructs a cache with `max_bytes` as the budget.
    pub fn with_size(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            current_bytes: 0,
            hits: 0,
            misses: 0,
            map: HashMap::new(),
            head: None,
            tail: None,
        }
    }

    /// `add`: caches `section` under `name`. Zero-byte or oversized
    /// sections are skipped; LRU entries are purged until it fits; an
    /// existing entry has payload and bytes replaced in place.
    pub fn add(&mut self, name: &str, bytes: usize, section: Arc<T>) {
        if bytes == 0 || bytes > self.max_bytes {
            return;
        }

        while self.head.is_some() && self.current_bytes + bytes > self.max_bytes {
            self.purge_lru();
        }

        if let Some(node) = self.map.get_mut(name) {
            self.current_bytes -= node.bytes;
            node.bytes = bytes;
            node.section = section;
            self.current_bytes += bytes;
            return;
        }

        let node = CacheNode {
            bytes,
            section,
            prev_to_newer: None,
            next_older: self.head.clone(),
        };
        if let Some(head) = &self.head {
            self.map.get_mut(head).unwrap().prev_to_newer = Some(name.to_string());
        } else {
            self.tail = Some(name.to_string());
        }
        self.head = Some(name.to_string());
        self.map.insert(name.to_string(), node);
        self.current_bytes += bytes;
    }

    /// `find`: hit returns the section and moves it to the head; miss
    /// counts a miss and returns `None`.
    pub fn find(&mut self, name: &str) -> Option<Arc<T>> {
        if self.map.contains_key(name) {
            self.move_to_head(name);
            self.hits += 1;
            self.map.get(name).map(|node| Arc::clone(&node.section))
        } else {
            self.misses += 1;
            None
        }
    }

    /// `remove`: drops one entry (the `BWResource::purge` core).
    pub fn remove(&mut self, name: &str) {
        if self.unlink(name) {
            // unlink leaves the totals alone (moveToHead shares it), so
            // the caller settles the bytes like the reference's remove.
            self.current_bytes -= self.map[name].bytes;
            self.map.remove(name);
        }
    }

    /// `clear`: empties the cache and resets every statistic.
    pub fn clear(&mut self) {
        self.map.clear();
        self.head = None;
        self.tail = None;
        self.current_bytes = 0;
        self.hits = 0;
        self.misses = 0;
    }

    /// `purgeLRU`: drops the least recently used entry.
    pub fn purge_lru(&mut self) {
        if let Some(tail) = self.tail.clone() {
            self.current_bytes -= self.map[&tail].bytes;
            self.unlink(&tail);
            self.map.remove(&tail);
        }
    }

    /// `moveToHead`.
    fn move_to_head(&mut self, name: &str) {
        if self.head.as_deref() == Some(name) {
            return;
        }
        self.unlink(name);
        let head = self.head.clone().unwrap();
        self.map.get_mut(&head).unwrap().prev_to_newer = Some(name.to_string());
        let node = self.map.get_mut(name).unwrap();
        node.prev_to_newer = None;
        node.next_older = Some(head);
        self.head = Some(name.to_string());
    }

    /// `unlinkNode`: detaches a node from the chain by position -- only
    /// node, head, tail, or middle. Byte totals are the callers' job (the
    /// reference subtracts in `remove`/`purgeLRU` only, so `moveToHead`'s
    /// relink keeps the accounting). Returns whether the node existed.
    fn unlink(&mut self, name: &str) -> bool {
        if !self.map.contains_key(name) {
            return false;
        }
        // Case 1: only node.
        if self.head.as_deref() == Some(name) && self.tail.as_deref() == Some(name) {
            self.head = None;
            self.tail = None;
        }
        // Case 2: head of the chain (a newer entry exists below it).
        else if self.head.as_deref() == Some(name) {
            let older = self.map[name].next_older.clone().unwrap();
            self.head = Some(older.clone());
            self.map.get_mut(&older).unwrap().prev_to_newer = None;
        }
        // Case 3: tail of the chain (a newer entry exists above it).
        else if self.tail.as_deref() == Some(name) {
            let newer = self.map[name].prev_to_newer.clone().unwrap();
            self.tail = Some(newer.clone());
            self.map.get_mut(&newer).unwrap().next_older = None;
        }
        // Case 4: in the middle; a non-head non-tail node always has both
        // neighbours, so rejoin them directly.
        else {
            let older = self.map[name].next_older.clone().unwrap();
            let newer = self.map[name].prev_to_newer.clone().unwrap();
            self.map.get_mut(&older).unwrap().prev_to_newer = Some(newer.clone());
            self.map.get_mut(&newer).unwrap().next_older = Some(older);
        }
        true
    }

    /// Current budget (`cache/maximum bytes`).
    pub fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    /// Bytes currently cached (`cache/current bytes`).
    pub fn current_bytes(&self) -> usize {
        self.current_bytes
    }

    /// Hit counter (`cache/hits`).
    pub fn hits(&self) -> usize {
        self.hits
    }

    /// Miss counter (`cache/misses`).
    pub fn misses(&self) -> usize {
        self.misses
    }

    /// Number of live entries.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Whether no entry is cached.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Chain order for tests/debugging: head (most recent) first.
    pub fn lru_order(&self) -> Vec<(String, usize)> {
        let mut order = Vec::with_capacity(self.map.len());
        let mut cursor = self.head.clone();
        while let Some(name) = cursor {
            let node = &self.map[&name];
            order.push((name.clone(), node.bytes));
            cursor = node.next_older.clone();
        }
        order
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section(tag: &str) -> Arc<String> {
        Arc::new(tag.to_string())
    }

    /// `find` with the payload cloned out, for terse assertions.
    fn find_str(cache: &mut DataSectionCache<String>, name: &str) -> Option<String> {
        cache.find(name).map(|section| (*section).clone())
    }

    #[test]
    fn zero_byte_and_oversized_sections_are_never_cached() {
        let mut cache = DataSectionCache::<String>::with_size(100);
        cache.add("empty.xml", 0, section("empty"));
        assert!(cache.is_empty());

        // Larger than the whole budget: skipped even though it is the only
        // candidate.
        cache.add("huge.xml", 101, section("huge"));
        assert!(cache.is_empty());
        assert_eq!(cache.current_bytes(), 0);
    }

    #[test]
    fn add_purges_lru_entries_until_the_newcomer_fits() {
        let mut cache = DataSectionCache::<String>::with_size(10);
        cache.add("a", 4, section("a"));
        cache.add("b", 4, section("b"));
        // Touch "a" so "b" becomes the least recently used.
        assert_eq!(find_str(&mut cache, "a").as_deref(), Some("a"));
        // Adding 4 more bytes only fits after one 4-byte purge, and the
        // purge takes the tail ("b"), not the touched "a".
        cache.add("c", 4, section("c"));

        assert_eq!(cache.len(), 2);
        assert_eq!(cache.current_bytes(), 8);
        assert_eq!(
            cache.lru_order(),
            vec![("c".to_string(), 4), ("a".to_string(), 4)]
        );
    }

    #[test]
    fn find_moves_hits_to_the_head_and_counts() {
        let mut cache = DataSectionCache::<String>::with_size(100);
        cache.add("a", 1, section("a"));
        cache.add("b", 2, section("b"));
        assert_eq!(
            cache.lru_order(),
            vec![("b".to_string(), 2), ("a".to_string(), 1)]
        );

        assert_eq!(find_str(&mut cache, "a").as_deref(), Some("a"));
        assert_eq!(
            cache.lru_order(),
            vec![("a".to_string(), 1), ("b".to_string(), 2)]
        );
        // Relinking to the head must not touch the byte accounting.
        assert_eq!(cache.current_bytes(), 3);
        assert!(find_str(&mut cache, "ghost").is_none());
        assert_eq!((cache.hits(), cache.misses()), (1, 1));

        // Heads stay put on repeated finds.
        assert_eq!(find_str(&mut cache, "a").as_deref(), Some("a"));
        assert_eq!(
            cache.lru_order(),
            vec![("a".to_string(), 1), ("b".to_string(), 2)]
        );
        assert_eq!(cache.current_bytes(), 3);
        assert_eq!(cache.hits(), 2);
    }

    #[test]
    fn re_adding_replaces_the_payload_in_place() {
        let mut cache = DataSectionCache::<String>::with_size(100);
        cache.add("a", 1, section("old"));
        cache.add("b", 2, section("b"));

        // Replace "a": value and bytes change, list position does not.
        cache.add("a", 3, section("new"));
        assert_eq!(cache.current_bytes(), 5);
        assert_eq!(
            cache.lru_order(),
            vec![("b".to_string(), 2), ("a".to_string(), 3)]
        );
        assert_eq!(find_str(&mut cache, "a").as_deref(), Some("new"));
    }

    #[test]
    fn remove_and_purge_lru_keep_the_chain_consistent() {
        let mut cache = DataSectionCache::<String>::with_size(100);
        cache.add("a", 1, section("a"));
        cache.add("b", 2, section("b"));
        cache.add("c", 3, section("c"));

        // Middle removal rejoins the neighbours.
        cache.remove("b");
        assert_eq!(cache.current_bytes(), 4);
        assert_eq!(
            cache.lru_order(),
            vec![("c".to_string(), 3), ("a".to_string(), 1)]
        );
        // Removing an absent name is a no-op.
        cache.remove("b");
        assert_eq!(cache.len(), 2);

        // purgeLRU takes the tail.
        cache.purge_lru();
        assert_eq!(cache.lru_order(), vec![("c".to_string(), 3)]);
        cache.purge_lru();
        assert!(cache.is_empty());
        // Purging an empty cache is a no-op.
        cache.purge_lru();
        assert_eq!(cache.current_bytes(), 0);

        // The chain still works after being emptied: re-add and walk.
        cache.add("d", 1, section("d"));
        assert_eq!(find_str(&mut cache, "d").as_deref(), Some("d"));
    }

    #[test]
    fn clear_resets_the_statistics_too() {
        let mut cache = DataSectionCache::<String>::with_size(100);
        cache.add("a", 1, section("a"));
        assert!(cache.find("a").is_some());
        assert!(cache.find("x").is_none());
        assert_eq!((cache.hits(), cache.misses()), (1, 1));

        cache.clear();
        assert!(cache.is_empty());
        assert_eq!(cache.current_bytes(), 0);
        // clear() zeroes the counters as well -- the stats are part of the
        // cache state in the reference.
        assert_eq!((cache.hits(), cache.misses()), (0, 0));
    }

    #[test]
    fn defaults_match_the_reference_budget() {
        assert_eq!(DEFAULT_CACHE_SIZE, 100 * 1024);
        let cache = DataSectionCache::<String>::default();
        assert_eq!(cache.max_bytes(), DEFAULT_CACHE_SIZE);
        assert!(cache.is_empty());
    }
}
