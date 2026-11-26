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

/// A chain slot. Slots live in the arena vector; freed slots are threaded
/// onto an intrusive free chain through `next_older` and reused by the
/// next `add`, so LRU relinks (`find` on a hit, purges) never allocate —
/// the reference's `moveToHead` relinks raw list nodes the same way.
/// `NONE` marks an absent neighbour.
struct CacheNode<T> {
    /// Path copy: `purgeLRU` knows only the slot index, and chain walks
    /// (`lru_order`) need names without a reverse map. Allocated once at
    /// `add`, alongside the map key.
    name: String,
    bytes: usize,
    /// `None` only while the slot sits on the free chain — the section is
    /// dropped when the entry is freed, like the reference deleting the
    /// node. Live slots always hold `Some`.
    section: Option<Arc<T>>,
    /// newer -> older; the head is the most recently used entry.
    prev_to_newer: usize,
    next_older: usize,
}

/// `DataSectionCache`: LRU of sections keyed by full resource path.
pub struct DataSectionCache<T> {
    max_bytes: usize,
    current_bytes: usize,
    hits: usize,
    misses: usize,
    /// Path -> arena slot. Each entry costs two Strings at `add` (the
    /// key and the slot's `name` copy) and nothing afterwards.
    map: HashMap<String, usize>,
    /// The arena. Slots are recycled through the free chain — chain
    /// indices stay valid because a slot is unlinked before it is freed.
    slots: Vec<CacheNode<T>>,
    /// Head of the intrusive free chain (dead slots threaded through
    /// `next_older`); `NONE` when no slot is free.
    free: usize,
    head: usize,
    tail: usize,
}

/// Absent neighbour / no entry.
const NONE: usize = usize::MAX;

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
            slots: Vec::new(),
            free: NONE,
            head: NONE,
            tail: NONE,
        }
    }

    /// `add`: caches `section` under `name`. Zero-byte or oversized
    /// sections are skipped; LRU entries are purged until it fits; an
    /// existing entry has payload and bytes replaced in place.
    pub fn add(&mut self, name: &str, bytes: usize, section: Arc<T>) {
        if bytes == 0 || bytes > self.max_bytes {
            return;
        }

        while self.head != NONE && self.current_bytes + bytes > self.max_bytes {
            self.purge_lru();
        }

        if let Some(&slot) = self.map.get(name) {
            let node = &mut self.slots[slot];
            self.current_bytes -= node.bytes;
            node.bytes = bytes;
            node.section = Some(section);
            self.current_bytes += bytes;
            return;
        }

        let slot = if self.free != NONE {
            let slot = self.free;
            self.free = self.slots[slot].next_older;
            let node = &mut self.slots[slot];
            node.name = name.to_string();
            node.bytes = bytes;
            node.section = Some(section);
            node.prev_to_newer = NONE;
            node.next_older = self.head;
            slot
        } else {
            self.slots.push(CacheNode {
                name: name.to_string(),
                bytes,
                section: Some(section),
                prev_to_newer: NONE,
                next_older: self.head,
            });
            self.slots.len() - 1
        };
        if self.head != NONE {
            self.slots[self.head].prev_to_newer = slot;
        } else {
            self.tail = slot;
        }
        self.head = slot;
        self.map.insert(name.to_string(), slot);
        self.current_bytes += bytes;
    }

    /// `find`: hit returns the section and moves it to the head; miss
    /// counts a miss and returns `None`.
    pub fn find(&mut self, name: &str) -> Option<Arc<T>> {
        let slot = match self.map.get(name) {
            Some(&slot) => slot,
            None => {
                self.misses += 1;
                return None;
            }
        };
        self.move_to_head(slot);
        self.hits += 1;
        Some(Arc::clone(
            self.slots[slot].section.as_ref().expect("live slot"),
        ))
    }

    /// `remove`: drops one entry (the `BWResource::purge` core).
    pub fn remove(&mut self, name: &str) {
        if let Some(&slot) = self.map.get(name) {
            // unlink leaves the totals alone (moveToHead shares it), so
            // the caller settles the bytes like the reference's remove.
            self.current_bytes -= self.slots[slot].bytes;
            self.unlink(slot);
            self.map.remove(name);
            self.free_slot(slot);
        }
    }

    /// `clear`: empties the cache and resets every statistic.
    pub fn clear(&mut self) {
        self.map.clear();
        self.slots.clear();
        self.free = NONE;
        self.head = NONE;
        self.tail = NONE;
        self.current_bytes = 0;
        self.hits = 0;
        self.misses = 0;
    }

    /// `purgeLRU`: drops the least recently used entry.
    pub fn purge_lru(&mut self) {
        if self.tail == NONE {
            return;
        }
        let tail = self.tail;
        self.current_bytes -= self.slots[tail].bytes;
        self.map.remove(self.slots[tail].name.as_str());
        self.unlink(tail);
        self.free_slot(tail);
    }

    /// `moveToHead`.
    fn move_to_head(&mut self, slot: usize) {
        if self.head == slot {
            return;
        }
        self.unlink(slot);
        let head = self.head;
        self.slots[head].prev_to_newer = slot;
        let node = &mut self.slots[slot];
        node.prev_to_newer = NONE;
        node.next_older = head;
        self.head = slot;
    }

    /// `unlinkNode`: detaches a node from the chain by position -- only
    /// node, head, tail, or middle. Byte totals are the callers' job (the
    /// reference subtracts in `remove`/`purgeLRU` only, so `moveToHead`'s
    /// relink keeps the accounting).
    fn unlink(&mut self, slot: usize) {
        // Case 1: only node.
        if self.head == slot && self.tail == slot {
            self.head = NONE;
            self.tail = NONE;
        }
        // Case 2: head of the chain (a newer entry exists below it).
        else if self.head == slot {
            let older = self.slots[slot].next_older;
            self.head = older;
            self.slots[older].prev_to_newer = NONE;
        }
        // Case 3: tail of the chain (a newer entry exists above it).
        else if self.tail == slot {
            let newer = self.slots[slot].prev_to_newer;
            self.tail = newer;
            self.slots[newer].next_older = NONE;
        }
        // Case 4: in the middle; a non-head non-tail node always has both
        // neighbours, so rejoin them directly.
        else {
            let older = self.slots[slot].next_older;
            let newer = self.slots[slot].prev_to_newer;
            self.slots[older].prev_to_newer = newer;
            self.slots[newer].next_older = older;
        }
    }

    /// Pushes a slot onto the free chain: clears the name (the capacity
    /// stays for the next resident), drops the section, and threads the
    /// slot through `next_older`. Never allocates.
    fn free_slot(&mut self, slot: usize) {
        let node = &mut self.slots[slot];
        node.name.clear();
        node.section = None;
        node.prev_to_newer = NONE;
        node.next_older = self.free;
        self.free = slot;
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
        let mut cursor = self.head;
        while cursor != NONE {
            let node = &self.slots[cursor];
            order.push((node.name.clone(), node.bytes));
            cursor = node.next_older;
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
