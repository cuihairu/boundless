//! BW-07-04-01: the backup hash and its target table.
//!
//! Every BaseApp owns a `BackupHash` (`lib/server/backup_hash.hpp`): it hashes
//! an entity id to a bucket of the *other* BaseApps that this app backs its
//! entities up to. `MiniBackupHash::hashFor` (`backup_hash.cpp:18`) multiplies
//! the id by a per-app random prime, takes the top 24 bits and folds them into
//! `virtualSize_` (the smallest power of two >= the number of buckets); a hash
//! that lands beyond the real bucket count folds back by `virtualSize_/2`. This
//! makes the buckets grow and shrink without disturbing most ids: adding a
//! bucket splits one original bucket in two, removing one merges two buckets
//! back. `BackupHash` adds the address table: `addressFor(id)` returns the
//! address of the bucket the id hashes to (`backup_hash.cpp:147`).
//!
//! `BackupHash::diff` (`backup_hash.cpp:163`) reports what changed between an
//! old and a new hash so that only affected backup apps need to be retold:
//! `onAdd` for a bucket present only in the new hash, `onChange` for a bucket
//! whose index, bucket width or prime changed, `onRemove` for a bucket only in
//! the old hash. `BackupHash::swap` (`backup_hash.hpp:117`) is how the
//! transition is committed once the new hash has been primed
//! (`BaseApp::useNewBackupHash`, `baseappmgr/baseapp.cpp:412`).
//!
//! `BackupHashChain` (`lib/server/backup_hash_chain.hpp`) remembers the hash of
//! every dead BaseApp so a mailbox pointing at a dead app can be followed to
//! its backup holder: `addressFor(addr, id)` (`backup_hash_chain.cpp:42`) walks
//! the history, substituting each dead address with where the id was backed up
//! to, until it reaches a live app (with a visited set to break cycles). It is
//! updated on a BaseApp death (`BaseAppMgr::onBaseAppDeath`,
//! `baseappmgr.cpp:1595`) and used to redirect global-base mailboxes and to
//! resolve mailboxes on a surviving BaseApp (`BaseApp::handleBaseAppDeath`,
//! `baseapp.cpp:2725`).
//!
//! The hashes and the chain stream to the recovery/notification streams
//! (`backup_hash.hpp:151` and `backup_hash_chain.cpp:93`): the hash streams its
//! prime and address table, the chain a `uint16` count of history entries.

use crate::baseappmgr::Addr;
use crate::entity_creator::EntityId;
use foundation::stream::{BinaryStream, StreamError};

/// `Mercury::Address::NONE` / the address returned for an empty hash
/// (`Mercury::Address( 0, 0 )`, `backup_hash.cpp:154`).
pub const HASH_NONE_ADDR: Addr = (0, 0);

/// The primes `BackupHash::choosePrime` picks from (`backup_hash.cpp:265`).
/// A random prime makes each BaseApp's hash function distinct, so a few dead
/// apps do not degenerate the distribution.
pub const PRIMES: [u32; 50] = [
    0x9e377e55, 0x9e377e53, 0x9e377e43, 0x9e377e41, 0x9e377e37, 0x9e377e11, 0x9e377e07, 0x9e377de1,
    0x9e377db7, 0x9e377da5, 0x9e377d99, 0x9e377d8f, 0x9e377d8d, 0x9e377d81, 0x9e377d65, 0x9e377d5f,
    0x9e377d53, 0x9e377d47, 0x9e377d21, 0x9e377d03, 0x9e377cff, 0x9e377cd3, 0x9e377c97, 0x9e377c75,
    0x9e377c6d, 0x9e377c3d, 0x9e377c33, 0x9e377c01, 0x9e377bcb, 0x9e377bbb, 0x9e377b91, 0x9e377b85,
    0x9e377b55, 0x9e377b0b, 0x9e377ae9, 0x9e377ae3, 0x9e377aab, 0x9e377aa7, 0x9e377a9f, 0x9e377a6f,
    0x9e377a5d, 0x9e377a45, 0x9e377a0b, 0x9e3779f3, 0x9e3779ed, 0x9e3779e5, 0x9e3779e1, 0x9e3779db,
    0x9e3779cd, 0x9e3779b1,
];

/// The id-to-bucket half of the hash: a prime and the current bucket count
/// (`MiniBackupHash`, `backup_hash.hpp:20`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MiniBackupHash {
    prime: u32,
    size: u32,
    virtual_size: u32,
}

impl MiniBackupHash {
    /// A hash with a given prime and bucket count.
    pub fn new(prime: u32, size: u32) -> Self {
        let mut hash = MiniBackupHash {
            prime,
            size: 0,
            virtual_size: 0,
        };
        hash.handle_size_change(size);
        hash
    }

    /// The prime (`prime()`).
    pub fn prime(&self) -> u32 {
        self.prime
    }

    /// The bucket count (`size()`).
    pub fn size(&self) -> u32 {
        self.size
    }

    /// The smallest power of two >= the bucket count (`virtualSize()`).
    pub fn virtual_size(&self) -> u32 {
        self.virtual_size
    }

    fn handle_size_change(&mut self, new_size: u32) {
        self.size = new_size;
        if new_size > 0 {
            self.virtual_size = 1;
            while self.virtual_size < new_size {
                self.virtual_size *= 2;
            }
        } else {
            self.virtual_size = 0;
        }
    }

    /// `MiniBackupHash::hashFor` (`backup_hash.cpp:18`): the bucket the id
    /// hashes to, or `u32::MAX` for an empty hash.
    pub fn hash_for(&self, id: EntityId) -> u32 {
        if self.size > 0 {
            // The lower bits do not tend to be as randomised.
            let mut hash = (id.wrapping_mul(self.prime) >> 8) % self.virtual_size;
            if hash >= self.size {
                hash -= self.virtual_size / 2;
            }
            return hash;
        }
        u32::MAX
    }

    /// `MiniBackupHash::virtualSizeFor` (`backup_hash.cpp:61`): the bucket
    /// width to use when considering a specific bucket. Buckets in the middle
    /// of the range are wider than the folded ones.
    pub fn virtual_size_for(&self, index: u32) -> u32 {
        let mini = self.virtual_size / 2;
        if (index < self.size - mini) || (mini <= index) {
            self.virtual_size
        } else {
            mini
        }
    }

    /// The `MiniBackupHash` stream format: `size`, then `prime`
    /// (`backup_hash.hpp:151`).
    pub fn write_to_stream(&self, stream: &mut BinaryStream) {
        stream.write_u32(self.size);
        stream.write_u32(self.prime);
    }

    /// Read a `MiniBackupHash` back from a stream (`backup_hash.hpp:158`).
    pub fn read_from_stream(stream: &mut BinaryStream) -> Result<Self, StreamError> {
        let size = stream.read_u32()?;
        let prime = stream.read_u32()?;
        Ok(Self::new(prime, size))
    }
}

/// The id-to-address table of one BaseApp's backup targets (`BackupHash`,
/// `backup_hash.hpp:78`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupHash {
    prime: u32,
    size: u32,
    virtual_size: u32,
    addrs: Vec<Addr>,
}

impl BackupHash {
    /// An empty hash with the default (first) prime.
    pub fn new() -> Self {
        Self::with_prime(Self::choose_prime(0))
    }

    /// An empty hash with an explicit prime (for deterministic tests).
    pub fn with_prime(prime: u32) -> Self {
        BackupHash {
            prime,
            size: 0,
            virtual_size: 0,
            addrs: Vec::new(),
        }
    }

    /// `BackupHash::choosePrime` (`backup_hash.cpp:263`): the prime to use for
    /// the `seed`-th draw of the prime table (`primes[seed % 50]`).
    pub fn choose_prime(seed: u32) -> u32 {
        PRIMES[(seed as usize) % PRIMES.len()]
    }

    /// The prime (`prime()`).
    pub fn prime(&self) -> u32 {
        self.prime
    }

    /// The bucket count (`size()`).
    pub fn size(&self) -> u32 {
        self.size
    }

    /// The smallest power of two >= the bucket count (`virtualSize()`).
    pub fn virtual_size(&self) -> u32 {
        self.virtual_size
    }

    fn handle_size_change(&mut self, new_size: u32) {
        self.size = new_size;
        if new_size > 0 {
            self.virtual_size = 1;
            while self.virtual_size < new_size {
                self.virtual_size *= 2;
            }
        } else {
            self.virtual_size = 0;
        }
    }

    /// `MiniBackupHash::virtualSizeFor` (`backup_hash.cpp:61`) for this table:
    /// the bucket width to consider for a specific bucket index.
    pub fn virtual_size_for(&self, index: u32) -> u32 {
        let mini = self.virtual_size / 2;
        if (index < self.size - mini) || (mini <= index) {
            self.virtual_size
        } else {
            mini
        }
    }

    /// `BackupHash::addressFor` (`backup_hash.cpp:147`): the address the id
    /// backs up to, or `HASH_NONE_ADDR` when the table is empty.
    pub fn address_for(&self, id: EntityId) -> Addr {
        if !self.addrs.is_empty() {
            return self.addrs[hash_for(self.prime, self.size, self.virtual_size, id)];
        }
        HASH_NONE_ADDR
    }

    /// `BackupHash::push_back` (`backup_hash.cpp:213`): add a backup target.
    pub fn push_back(&mut self, addr: Addr) {
        self.addrs.push(addr);
        self.handle_size_change(self.addrs.len() as u32);
    }

    /// `BackupHash::erase` (`backup_hash.cpp:224`): remove a target with a
    /// swap-with-last so the hash changes as little as possible. Returns
    /// whether the address was present.
    pub fn erase(&mut self, addr: Addr) -> bool {
        if let Some(index) = self.addrs.iter().position(|&a| a == addr) {
            self.addrs[index] = *self.addrs.last().unwrap();
            self.addrs.pop();
            self.handle_size_change(self.addrs.len() as u32);
            return true;
        }
        false
    }

    /// `BackupHash::clearAddress` (`backup_hash.cpp:244`): mark a target as
    /// gone without shrinking the table (used while retiring). Returns whether
    /// the address was present.
    pub fn clear_address(&mut self, addr: Addr) -> bool {
        if let Some(slot) = self.addrs.iter_mut().find(|a| **a == addr) {
            *slot = HASH_NONE_ADDR;
            return true;
        }
        false
    }

    /// `BackupHash::clear` (`backup_hash.hpp:105`): empty the table.
    pub fn clear(&mut self) {
        self.addrs.clear();
        self.handle_size_change(0);
    }

    /// Whether there are no backup targets (`empty()`).
    pub fn is_empty(&self) -> bool {
        self.addrs.is_empty()
    }

    /// The number of buckets (`size()`).
    pub fn len(&self) -> u32 {
        self.addrs.len() as u32
    }

    /// The bucket table, indexed by `hash_for`.
    pub fn addresses(&self) -> &[Addr] {
        &self.addrs
    }

    /// `BackupHash::swap` (`backup_hash.hpp:117`): commit a primed hash in
    /// place of the current one.
    pub fn swap(&mut self, other: &mut BackupHash) {
        std::mem::swap(&mut self.addrs, &mut other.addrs);
        std::mem::swap(&mut self.prime, &mut other.prime);
        self.handle_size_change(self.addrs.len() as u32);
        other.handle_size_change(other.addrs.len() as u32);
    }

    /// `BackupHash::diff` (`backup_hash.cpp:163`): report every bucket that
    /// differs between `self` (the old hash) and `other` (the new hash).
    pub fn diff(&self, other: &BackupHash, visitor: &mut impl BackupHashDiffVisitor) {
        let prime_changed = other.prime != self.prime;

        // The old addresses, by bucket index.
        let mut src_index = std::collections::BTreeMap::new();
        for (index, addr) in self.addrs.iter().enumerate() {
            src_index.insert(*addr, index as u32);
        }

        for (index, addr) in other.addrs.iter().enumerate() {
            let index = index as u32;
            if let Some(&old_index) = src_index.get(addr) {
                if index != old_index
                    || other.virtual_size_for(index) != self.virtual_size_for(index)
                    || prime_changed
                {
                    visitor.on_change(*addr, index, other.virtual_size_for(index), other.prime);
                }
                src_index.remove(addr);
            } else {
                visitor.on_add(*addr, index, other.virtual_size_for(index), other.prime);
            }
        }

        for (addr, index) in src_index {
            visitor.on_remove(addr, index, self.virtual_size_for(index), self.prime);
        }
    }

    /// The `BackupHash` stream format: `prime`, then the count and the address
    /// table (`backup_hash.hpp:166`).
    pub fn write_to_stream(&self, stream: &mut BinaryStream) {
        stream.write_u32(self.prime);
        stream.write_u32(self.addrs.len() as u32);
        for addr in &self.addrs {
            write_addr(stream, *addr);
        }
    }

    /// Read a `BackupHash` back from a stream (`backup_hash.hpp:172`).
    pub fn read_from_stream(stream: &mut BinaryStream) -> Result<Self, StreamError> {
        let prime = stream.read_u32()?;
        let count = stream.read_u32()?;
        let mut hash = Self::with_prime(prime);
        for _ in 0..count {
            hash.push_back(read_addr(stream)?);
        }
        Ok(hash)
    }
}

impl Default for BackupHash {
    fn default() -> Self {
        Self::new()
    }
}

/// The bucket width of an id under a given `(prime, size)` pair.
fn hash_for(prime: u32, size: u32, virtual_size: u32, id: EntityId) -> usize {
    let mut hash = (id.wrapping_mul(prime) >> 8) % virtual_size;
    if hash >= size {
        hash -= virtual_size / 2;
    }
    hash as usize
}

/// The visitor `BackupHash::diff` drives (`backup_hash.hpp:91`).
pub trait BackupHashDiffVisitor {
    /// A bucket present only in the new hash.
    fn on_add(&mut self, addr: Addr, index: u32, virtual_size: u32, prime: u32);
    /// A bucket present in both hashes but at a different index, width or prime.
    fn on_change(&mut self, addr: Addr, index: u32, virtual_size: u32, prime: u32);
    /// A bucket present only in the old hash.
    fn on_remove(&mut self, addr: Addr, index: u32, virtual_size: u32, prime: u32);
}

/// The history of dead BaseApp hashes (`BackupHashChain`,
/// `backup_hash_chain.hpp:14`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackupHashChain {
    history: std::collections::BTreeMap<Addr, BackupHash>,
}

impl BackupHashChain {
    /// An empty chain.
    pub fn new() -> Self {
        Self::default()
    }

    /// `BackupHashChain::adjustForDeadBaseApp` (`backup_hash_chain.cpp:31`):
    /// remember the dead app's hash so its mailboxes can be redirected.
    pub fn adjust_for_dead_base_app(&mut self, dead_app: Addr, hash: BackupHash) {
        self.history.insert(dead_app, hash);
    }

    /// `BackupHashChain::addressFor` (`backup_hash_chain.cpp:42`): follow the
    /// dead-app trail until a live app is reached (or the original address, if
    /// no crash is recorded for it). A visited set breaks cycles.
    pub fn address_for(&self, address: Addr, entity_id: EntityId) -> Addr {
        // Quickly look up the address to see if it needs redirection.
        let mut match_entry = match self.history.get(&address) {
            Some(hash) => hash,
            None => return address,
        };

        let mut visited = std::collections::BTreeSet::new();
        let mut position = address;

        loop {
            if !visited.insert(position) {
                // Infinite loop: give up and keep the original address.
                return address;
            }
            position = match_entry.address_for(entity_id);
            match_entry = match self.history.get(&position) {
                Some(hash) => hash,
                None => return position,
            };
        }
    }

    /// Whether no BaseApp deaths have been recorded.
    pub fn is_empty(&self) -> bool {
        self.history.is_empty()
    }

    /// The number of recorded dead BaseApps.
    pub fn len(&self) -> usize {
        self.history.len()
    }

    /// The dead BaseApp hashes, by dead-app address.
    pub fn history(&self) -> &std::collections::BTreeMap<Addr, BackupHash> {
        &self.history
    }

    /// The chain stream format: a `uint16` count, then `(addr, hash)` pairs
    /// (`backup_hash_chain.cpp:93`).
    pub fn write_to_stream(&self, stream: &mut BinaryStream) {
        stream.write_u16(self.history.len() as u16);
        for (addr, hash) in &self.history {
            write_addr(stream, *addr);
            hash.write_to_stream(stream);
        }
    }

    /// Read a chain back from a stream (`backup_hash_chain.cpp:118`).
    pub fn read_from_stream(stream: &mut BinaryStream) -> Result<Self, StreamError> {
        let count = stream.read_u16()?;
        let mut chain = Self::new();
        for _ in 0..count {
            let addr = read_addr(stream)?;
            let hash = BackupHash::read_from_stream(stream)?;
            chain.history.insert(addr, hash);
        }
        Ok(chain)
    }
}

/// A `Mercury::Address` on a stream: `ip` then `port`.
fn write_addr(stream: &mut BinaryStream, addr: Addr) {
    stream.write_u32(addr.0);
    stream.write_u16(addr.1);
}

/// Read a `Mercury::Address` from a stream.
fn read_addr(stream: &mut BinaryStream) -> Result<Addr, StreamError> {
    let ip = stream.read_u32()?;
    let port = stream.read_u16()?;
    Ok((ip, port))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `hashFor` reference literals (prime `0x9e377e55`), hand-computed from
    /// `backup_hash.cpp:18`.
    #[test]
    fn hash_for_matches_the_reference_formula() {
        let prime = 0x9e377e55;
        let hash = MiniBackupHash::new(prime, 6);
        assert_eq!(hash.size(), 6);
        assert_eq!(hash.virtual_size(), 8);
        // Buckets 1 and 3 fold onto the same bucket 2 (the split bucket).
        assert_eq!(hash.hash_for(1), 2);
        assert_eq!(hash.hash_for(2), 4);
        assert_eq!(hash.hash_for(3), 2);
    }

    /// The bucket count grows and shrinks without disturbing most ids: the
    /// hash folds back by `virtualSize_/2` when it lands past the real count.
    #[test]
    fn the_bucket_count_folds_back_to_the_real_size() {
        let prime = 0x9e377e55;
        let one = MiniBackupHash::new(prime, 1);
        assert_eq!(one.hash_for(5), 0);
        assert_eq!(MiniBackupHash::new(prime, 2).hash_for(5), 1);
        assert_eq!(MiniBackupHash::new(prime, 3).hash_for(5), 1);
        assert_eq!(MiniBackupHash::new(prime, 7).hash_for(5), 3);
        assert_eq!(MiniBackupHash::new(prime, 8).hash_for(5), 7);
        assert_eq!(MiniBackupHash::new(prime, 9).hash_for(5), 7);
    }

    /// An empty hash has no bucket (`u32::MAX`).
    #[test]
    fn an_empty_hash_has_no_bucket() {
        let hash = MiniBackupHash::new(0x9e377e55, 0);
        assert_eq!(hash.virtual_size(), 0);
        assert_eq!(hash.hash_for(5), u32::MAX);
    }

    /// `virtualSizeFor` reference literals: the middle buckets are the folded,
    /// wider ones (`backup_hash.cpp:61`).
    #[test]
    fn virtual_size_for_matches_the_reference_formula() {
        let prime = 0x9e377e55;
        let six = MiniBackupHash::new(prime, 6);
        assert_eq!(
            (0..6).map(|i| six.virtual_size_for(i)).collect::<Vec<_>>(),
            vec![8, 8, 4, 4, 8, 8]
        );
        let three = MiniBackupHash::new(prime, 3);
        assert_eq!(
            (0..3)
                .map(|i| three.virtual_size_for(i))
                .collect::<Vec<_>>(),
            vec![4, 2, 4]
        );
        let seven = MiniBackupHash::new(prime, 7);
        assert_eq!(
            (0..7)
                .map(|i| seven.virtual_size_for(i))
                .collect::<Vec<_>>(),
            vec![8, 8, 8, 4, 8, 8, 8]
        );
    }

    /// `addressFor` returns the address of the bucket the id hashes to.
    #[test]
    fn address_for_returns_the_bucket_address() {
        let mut hash = BackupHash::with_prime(0x9e377e55);
        hash.push_back((1, 1000));
        hash.push_back((1, 2000));
        hash.push_back((2, 3000));
        hash.push_back((2, 4000));
        hash.push_back((3, 5000));
        hash.push_back((3, 6000));
        assert_eq!(hash.address_for(1), (2, 3000)); // bucket 2
        assert_eq!(hash.address_for(2), (3, 5000)); // bucket 4
        assert_eq!(hash.address_for(3), (2, 3000)); // bucket 2
    }

    /// An empty hash maps every id to the "no address" sentinel.
    #[test]
    fn an_empty_hash_maps_to_none() {
        let hash = BackupHash::with_prime(0x9e377e55);
        assert!(hash.is_empty());
        assert_eq!(hash.address_for(7), HASH_NONE_ADDR);
    }

    /// `erase` swaps with the last bucket and shrinks the table; a missing
    /// address is a no-op.
    #[test]
    fn erase_swaps_with_the_last_bucket() {
        let mut hash = BackupHash::with_prime(0x9e377e55);
        hash.push_back((1, 1000));
        hash.push_back((1, 2000));
        hash.push_back((1, 3000));
        assert_eq!(hash.addresses(), &[(1, 1000), (1, 2000), (1, 3000)]);

        assert!(hash.erase((1, 1000)));
        assert_eq!(hash.addresses(), &[(1, 3000), (1, 2000)]);
        assert_eq!(hash.size(), 2);
        assert!(!hash.erase((9, 9)));
        assert_eq!(hash.size(), 2);
    }

    /// `clearAddress` marks a bucket gone without shrinking the table.
    #[test]
    fn clear_address_marks_a_bucket_gone() {
        let mut hash = BackupHash::with_prime(0x9e377e55);
        hash.push_back((1, 1000));
        hash.push_back((1, 2000));
        assert!(hash.clear_address((1, 1000)));
        assert_eq!(hash.addresses(), &[HASH_NONE_ADDR, (1, 2000)]);
        assert_eq!(hash.size(), 2, "the table does not shrink");
        assert!(!hash.clear_address((9, 9)));
    }

    /// `swap` commits a primed hash in place, keeping the addresses and the
    /// prime of the new hash.
    #[test]
    fn swap_commits_the_new_hash() {
        let mut old = BackupHash::with_prime(0x9e377e55);
        old.push_back((1, 1000));
        let mut new = BackupHash::with_prime(0x9e377e53);
        new.push_back((2, 2000));
        new.push_back((3, 3000));

        old.swap(&mut new);
        assert_eq!(old.addresses(), &[(2, 2000), (3, 3000)]);
        assert_eq!(old.prime(), 0x9e377e53);
        assert_eq!(old.size(), 2);
        assert_eq!(new.addresses(), &[(1, 1000)]);
        assert_eq!(new.prime(), 0x9e377e55);
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct DiffRecord {
        kind: &'static str,
        addr: Addr,
        index: u32,
        virtual_size: u32,
        prime: u32,
    }

    struct RecordingVisitor(Vec<DiffRecord>);

    impl BackupHashDiffVisitor for RecordingVisitor {
        fn on_add(&mut self, addr: Addr, index: u32, virtual_size: u32, prime: u32) {
            self.0.push(DiffRecord {
                kind: "add",
                addr,
                index,
                virtual_size,
                prime,
            });
        }
        fn on_change(&mut self, addr: Addr, index: u32, virtual_size: u32, prime: u32) {
            self.0.push(DiffRecord {
                kind: "change",
                addr,
                index,
                virtual_size,
                prime,
            });
        }
        fn on_remove(&mut self, addr: Addr, index: u32, virtual_size: u32, prime: u32) {
            self.0.push(DiffRecord {
                kind: "remove",
                addr,
                index,
                virtual_size,
                prime,
            });
        }
    }

    /// `diff` reports only what changed: added buckets, a removed bucket and a
    /// bucket whose width changed when the count crossed a power of two.
    #[test]
    fn diff_reports_add_change_and_remove() {
        let prime = 0x9e377e55;
        let mut old = BackupHash::with_prime(prime);
        for addr in [(1, 1000), (1, 2000), (1, 3000)] {
            old.push_back(addr);
        }
        let mut new = BackupHash::with_prime(prime);
        for addr in [(1, 1000), (1, 2000), (2, 4000), (2, 5000)] {
            new.push_back(addr);
        }

        let mut visitor = RecordingVisitor(Vec::new());
        old.diff(&new, &mut visitor);

        // A at index 0 is unchanged. B at index 1 widens (size 3 -> 4). D and E
        // are added at indices 2 and 3. C is removed at index 2.
        assert_eq!(
            visitor.0,
            vec![
                DiffRecord {
                    kind: "change",
                    addr: (1, 2000),
                    index: 1,
                    virtual_size: 4,
                    prime,
                },
                DiffRecord {
                    kind: "add",
                    addr: (2, 4000),
                    index: 2,
                    virtual_size: 4,
                    prime,
                },
                DiffRecord {
                    kind: "add",
                    addr: (2, 5000),
                    index: 3,
                    virtual_size: 4,
                    prime,
                },
                DiffRecord {
                    kind: "remove",
                    addr: (1, 3000),
                    index: 2,
                    virtual_size: 4,
                    prime,
                },
            ]
        );
    }

    /// A changed prime marks every common bucket as changed.
    #[test]
    fn a_changed_prime_changes_every_bucket() {
        let mut old = BackupHash::with_prime(0x9e377e55);
        old.push_back((1, 1000));
        let mut new = BackupHash::with_prime(0x9e377e53);
        new.push_back((1, 1000));

        let mut visitor = RecordingVisitor(Vec::new());
        old.diff(&new, &mut visitor);
        assert_eq!(visitor.0.len(), 1);
        assert_eq!(visitor.0[0].kind, "change");
        assert_eq!(visitor.0[0].prime, 0x9e377e53);
    }

    /// The chain resolves a dead app's address to where the id was backed up.
    #[test]
    fn the_chain_follows_a_dead_app_to_its_backup() {
        let mut chain = BackupHashChain::new();
        let mut dead_hash = BackupHash::with_prime(0x9e377e55);
        dead_hash.push_back((2, 2000));
        chain.adjust_for_dead_base_app((1, 1000), dead_hash);

        // The id is backed up to the surviving app.
        assert_eq!(chain.address_for((1, 1000), 1), (2, 2000));
        // An address with no recorded crash is returned unchanged.
        assert_eq!(chain.address_for((2, 2000), 1), (2, 2000));
    }

    /// The chain follows a trail of dead apps to the first live one.
    #[test]
    fn the_chain_follows_a_trail_to_a_live_app() {
        let mut chain = BackupHashChain::new();
        let mut hash_a = BackupHash::with_prime(0x9e377e55);
        hash_a.push_back((2, 2000));
        chain.adjust_for_dead_base_app((1, 1000), hash_a);
        let mut hash_b = BackupHash::with_prime(0x9e377e53);
        hash_b.push_back((3, 3000));
        chain.adjust_for_dead_base_app((2, 2000), hash_b);

        assert_eq!(chain.address_for((1, 1000), 1), (3, 3000));
        assert_eq!(chain.len(), 2);
    }

    /// A cycle in the dead-app trail is broken and the original address kept.
    #[test]
    fn the_chain_breaks_a_cycle() {
        let mut chain = BackupHashChain::new();
        let mut hash_a = BackupHash::with_prime(0x9e377e55);
        hash_a.push_back((2, 2000));
        chain.adjust_for_dead_base_app((1, 1000), hash_a);
        let mut hash_b = BackupHash::with_prime(0x9e377e53);
        hash_b.push_back((1, 1000));
        chain.adjust_for_dead_base_app((2, 2000), hash_b);

        assert_eq!(chain.address_for((1, 1000), 1), (1, 1000));
    }

    /// The chain, hash and mini-hash stream round-trip through the
    /// recovery/notification stream formats.
    #[test]
    fn hashes_and_the_chain_round_trip_through_a_stream() {
        let mut chain = BackupHashChain::new();
        let mut dead_hash = BackupHash::with_prime(0x9e377e55);
        dead_hash.push_back((2, 2000));
        dead_hash.push_back((2, 3000));
        chain.adjust_for_dead_base_app((1, 1000), dead_hash);

        let mut stream = BinaryStream::new();
        chain.write_to_stream(&mut stream);
        let restored = BackupHashChain::read_from_stream(&mut stream).expect("read chain");
        assert_eq!(restored, chain);
        assert_eq!(restored.address_for((1, 1000), 1), (2, 2000));

        let mut hash = BackupHash::with_prime(0x9e377e53);
        hash.push_back((3, 3000));
        let mut stream = BinaryStream::new();
        hash.write_to_stream(&mut stream);
        assert_eq!(
            BackupHash::read_from_stream(&mut stream).expect("read hash"),
            hash
        );

        let mut stream = BinaryStream::new();
        MiniBackupHash::new(0x9e377e55, 6).write_to_stream(&mut stream);
        let mini = MiniBackupHash::read_from_stream(&mut stream).expect("read mini");
        assert_eq!(mini.size(), 6);
        assert_eq!(mini.virtual_size(), 8);
        assert_eq!(mini.prime(), 0x9e377e55);
    }
}
