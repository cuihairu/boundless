//! The per-type object buckets an intersect query fills
//! (`lib/scene/intersection_set.{hpp,cpp}`).
//!
//! The reference buckets through `LookUpTable<SceneObjectList>`, whose
//! `operator[]` grows the table to fit the index
//! (`cstdmf/lookup_table.hpp:106-115`); the port grows the same way on
//! insert. One registered deviation: the reference's **const** `at` also
//! grows the table through a mutable member, so reading an out-of-range
//! type id inflates `maxType`; the port answers an empty slice and leaves
//! the bucket count alone.

use crate::scene_object::SceneObject;
use crate::type_system::RuntimeTypeID;

/// `IntersectionSet` (`intersection_set.hpp:17-34`).
#[derive(Debug, Clone, Default)]
pub struct IntersectionSet {
    buckets: Vec<Vec<SceneObject>>,
}

impl IntersectionSet {
    /// `IntersectionSet()` — no pre-sized buckets.
    pub fn new() -> Self {
        Self::default()
    }

    /// `IntersectionSet(typeMax)` (`intersection_set.cpp:16-18`): the
    /// reference `LookUpTable` **reserves** without changing `size()`, so
    /// `maxType` still answers 0 until an insert grows the table; the port
    /// mirrors that by remembering only a capacity hint.
    pub fn with_type_max(type_max: RuntimeTypeID) -> Self {
        let mut set = Self::default();
        set.buckets.reserve(type_max as usize);
        set
    }

    /// `insert` (`intersection_set.cpp:28-31`): grow to fit, append.
    pub fn insert(&mut self, object: SceneObject) {
        let type_id = object.type_id() as usize;
        if type_id >= self.buckets.len() {
            self.buckets.resize(type_id + 1, Vec::new());
        }
        self.buckets[type_id].push(object);
    }

    /// `objectsOfType(typeID)` const face (`intersection_set.cpp:39-41`).
    pub fn objects_of_type(&self, type_id: RuntimeTypeID) -> &[SceneObject] {
        self.buckets
            .get(type_id as usize)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// `objectsOfType(typeID)` mutable face (`intersection_set.cpp:33-37`):
    /// grows to fit like the reference `operator[]`.
    pub fn objects_of_type_mut(&mut self, type_id: RuntimeTypeID) -> &mut Vec<SceneObject> {
        let index = type_id as usize;
        if index >= self.buckets.len() {
            self.buckets.resize(index + 1, Vec::new());
        }
        &mut self.buckets[index]
    }

    /// `clear` (`intersection_set.cpp:49-56`): every bucket truncated, the
    /// bucket count untouched.
    pub fn clear(&mut self) {
        for bucket in &mut self.buckets {
            bucket.clear();
        }
    }

    /// `numObjects` (`intersection_set.cpp:58-67`).
    pub fn num_objects(&self) -> usize {
        self.buckets.iter().map(Vec::len).sum()
    }

    /// `maxType` (`intersection_set.cpp:69-72`): the bucket count as a
    /// runtime type id.
    pub fn max_type(&self) -> RuntimeTypeID {
        self.buckets.len() as RuntimeTypeID
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene_object_flags::SceneObjectFlags;

    struct Unit;
    struct Bullet;

    fn object<T: ?Sized + 'static>(handle: u64) -> SceneObject {
        SceneObject::typed::<T>(handle, SceneObjectFlags::new())
    }

    #[test]
    fn inserts_grow_the_buckets_and_answer_by_type() {
        let mut set = IntersectionSet::new();
        assert_eq!(set.max_type(), 0);
        assert_eq!(set.num_objects(), 0);

        let unit_type = SceneObject::type_of::<Unit>();
        let bullet_type = SceneObject::type_of::<Bullet>();

        set.insert(object::<Unit>(1));
        set.insert(object::<Bullet>(2));
        set.insert(object::<Unit>(3));

        // Buckets grow to fit the largest index (checkIndex, lookup_table.hpp:106-115):
        // the count is inflated by never-inserted holes below the id.
        assert_eq!(
            set.max_type() as usize,
            unit_type.max(bullet_type) as usize + 1
        );
        assert_eq!(set.num_objects(), 3);
        assert_eq!(
            set.objects_of_type(unit_type)
                .iter()
                .map(SceneObject::handle)
                .collect::<Vec<_>>(),
            [1, 3]
        );
        assert_eq!(
            set.objects_of_type(bullet_type)
                .iter()
                .map(SceneObject::handle)
                .collect::<Vec<_>>(),
            [2]
        );
        // A never-inserted type answers an empty slice.
        assert!(set.objects_of_type(9).is_empty());
    }

    #[test]
    fn clear_truncates_buckets_but_keeps_their_count() {
        let unit_type = SceneObject::type_of::<Unit>();
        let bullet_type = SceneObject::type_of::<Bullet>();
        let mut set = IntersectionSet::with_type_max(4);
        set.insert(object::<Unit>(1));
        set.insert(object::<Bullet>(2));
        set.clear();
        assert_eq!(set.num_objects(), 0);
        assert_eq!(
            set.max_type() as usize,
            unit_type.max(bullet_type) as usize + 1,
            "clear does not shrink the table"
        );
        assert!(set.objects_of_type(unit_type).is_empty());

        // The mutable face grows like the reference operator[].
        let bucket = set.objects_of_type_mut(bullet_type);
        bucket.push(object::<Bullet>(9));
        assert_eq!(set.num_objects(), 1);

        // A far-out type grows the table on the mutable face too.
        set.objects_of_type_mut(250).push(object::<Bullet>(10));
        assert_eq!(set.num_objects(), 2);
        assert_eq!(set.max_type() as usize, 251);
    }
}
