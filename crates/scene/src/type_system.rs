//! The scene runtime type system (`lib/scene/scene_type_system.{hpp,cpp}`)
//! and the `bw_hash` primitives it builds on (`lib/cstdmf/bw_hash.{hpp,cpp}`).
//!
//! Type identity comes from `std::any::type_name` in place of the
//! reference's `typeid(T*).name()` — the numeric ids differ from the C++
//! build, but the in-process uniqueness and stability semantics are the
//! same (registered adaptation). The reference caches each id in a
//! function-local static; here ids are recomputed per call, which is
//! idempotent because every lookup hashes the same name into the same
//! context.

use std::any::type_name;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

/// `SceneTypeSystem::RuntimeTypeID` (`scene_type_system.hpp:16`).
pub type RuntimeTypeID = u8;

/// `SceneTypeSystem::GlobalUniqueTypeID` (`scene_type_system.hpp:17`).
pub type GlobalUniqueTypeID = u64;

/// `TypeIDContext::UNKNOWN` (`scene_type_system.hpp:37`).
pub const UNKNOWN: RuntimeTypeID = 0;

/// 64-bit FNV-1 constants — on el7 x86-64 `size_t` is 8 bytes
/// (`bw_hash.cpp:14-15`).
const FNV_OFFSET_BASIS: u64 = 14695981039346656037;
const FNV_PRIME: u64 = 1099511628211;

/// `hash_string(const void*, size_t)` (`bw_hash.cpp:24-36`): FNV-1 over
/// the raw bytes.
pub fn hash_string(bytes: &[u8]) -> u64 {
    let mut result = FNV_OFFSET_BASIS;
    for &byte in bytes {
        result = (result ^ byte as u64).wrapping_mul(FNV_PRIME);
    }
    result
}

/// `hash_combine` (`bw_hash.hpp:23-37`): `seed ^= hash(v) + 0xBC6EF372 +
/// (seed << 5) + (seed >> 3)`. Callers pre-hash their values; this port
/// takes the already-hashed `u64`.
pub fn hash_combine(seed: &mut u64, value: u64) {
    *seed ^= value
        .wrapping_add(0xBC6EF372)
        .wrapping_add(*seed << 5)
        .wrapping_add(*seed >> 3);
}

/// `TypeIDContext` (`scene_type_system.hpp:31-49`): remaps global unique
/// ids into a local set of contiguous ids that fit lookup tables.
#[derive(Default)]
pub struct TypeIDContext {
    internal_mapping: HashMap<GlobalUniqueTypeID, RuntimeTypeID>,
    external_mapping: Vec<GlobalUniqueTypeID>,
}

impl TypeIDContext {
    /// `getLocalID` (`scene_type_system.cpp:63-86`): ids are 1-based so
    /// that `UNKNOWN` (0) can mean "no mapping"; running out of `u8`
    /// storage is an assert, as in the reference.
    pub fn get_local_id(&mut self, global_unique_id: GlobalUniqueTypeID) -> RuntimeTypeID {
        if let Some(&id) = self.internal_mapping.get(&global_unique_id) {
            return id;
        }

        self.external_mapping.push(global_unique_id);
        let result = self.external_mapping.len() as RuntimeTypeID;
        self.internal_mapping.insert(global_unique_id, result);

        assert!(result != UNKNOWN, "type id storage exhausted");

        result
    }

    /// `getGlobalID` (`scene_type_system.cpp:88-101`): out-of-range local
    /// ids answer `UNKNOWN` (0).
    pub fn get_global_id(&self, local_unique_id: RuntimeTypeID) -> GlobalUniqueTypeID {
        if (local_unique_id.wrapping_sub(1) as usize) >= self.external_mapping.len() {
            return UNKNOWN as GlobalUniqueTypeID;
        }
        self.external_mapping[local_unique_id as usize - 1]
    }
}

static SCENE_GLOBAL_TYPE_CONTEXT: LazyLock<Mutex<TypeIDContext>> =
    LazyLock::new(|| Mutex::new(TypeIDContext::default()));
static VIEW_TYPE_CONTEXT: LazyLock<Mutex<TypeIDContext>> =
    LazyLock::new(|| Mutex::new(TypeIDContext::default()));
static OBJECT_TYPE_CONTEXT: LazyLock<Mutex<TypeIDContext>> =
    LazyLock::new(|| Mutex::new(TypeIDContext::default()));
static OBJECT_OPERATION_TYPE_CONTEXT: LazyLock<Mutex<TypeIDContext>> =
    LazyLock::new(|| Mutex::new(TypeIDContext::default()));
static TYPE_NAME_LOOKUP: LazyLock<Mutex<HashMap<u64, &'static str>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// `generateSceneTypeID` (`scene_type_system.cpp:25-40`): hash the name,
/// map it through the global context, and record the name for
/// `fetchTypeName`.
pub fn generate_scene_type_id(type_name_static: &'static str) -> GlobalUniqueTypeID {
    let hash_id = hash_string(type_name_static.as_bytes());

    let scene_id = SCENE_GLOBAL_TYPE_CONTEXT
        .lock()
        .unwrap()
        .get_local_id(hash_id);

    TYPE_NAME_LOOKUP
        .lock()
        .unwrap()
        .insert(scene_id as u64, type_name_static);

    scene_id as GlobalUniqueTypeID
}

/// `fetchTypeName` (`scene_type_system.cpp:42-46`): the reference looks a
/// default-constructed (NULL) entry up on a miss; the port answers `None`.
pub fn fetch_type_name(id: GlobalUniqueTypeID) -> Option<&'static str> {
    TYPE_NAME_LOOKUP.lock().unwrap().get(&id).copied()
}

/// `GloballyUniqueTypeID<T>::getID` — the reference hashes
/// `typeid(T*).name()`; the port hashes `type_name::<T>()`.
pub fn globally_unique_type_id<T: ?Sized + 'static>() -> GlobalUniqueTypeID {
    generate_scene_type_id(type_name::<T>())
}

/// `getSceneViewRuntimeID<T>` (`scene_type_system.hpp:102-107`).
pub fn scene_view_runtime_id<T: ?Sized + 'static>() -> RuntimeTypeID {
    let type_id = globally_unique_type_id::<T>();
    VIEW_TYPE_CONTEXT.lock().unwrap().get_local_id(type_id)
}

/// `getObjectRuntimeID<T>` (`scene_type_system.hpp:109-114`).
pub fn object_runtime_id<T: ?Sized + 'static>() -> RuntimeTypeID {
    let type_id = globally_unique_type_id::<T>();
    OBJECT_TYPE_CONTEXT.lock().unwrap().get_local_id(type_id)
}

/// `sceneObjectTypeIDContext()` (`scene_type_system.cpp:53-56`): guarded
/// access for name resolution (`SceneObject::typeName`).
pub fn object_context() -> std::sync::MutexGuard<'static, TypeIDContext> {
    OBJECT_TYPE_CONTEXT.lock().unwrap()
}

/// `getObjectOperationRuntimeID<T>` (`scene_type_system.hpp:116-121`).
pub fn object_operation_runtime_id<T: ?Sized + 'static>() -> RuntimeTypeID {
    let type_id = globally_unique_type_id::<T>();
    OBJECT_OPERATION_TYPE_CONTEXT
        .lock()
        .unwrap()
        .get_local_id(type_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TypeA;
    struct TypeB;

    #[test]
    fn hash_string_is_fnv1_over_the_bytes() {
        // The FNV-1 chain is pinned against a hand-rolled fold of the same
        // constants: "" keeps the offset basis, "a" is one round.
        assert_eq!(hash_string(b""), FNV_OFFSET_BASIS);
        assert_eq!(
            hash_string(b"a"),
            (FNV_OFFSET_BASIS ^ 97).wrapping_mul(FNV_PRIME)
        );
        let mut expected = FNV_OFFSET_BASIS;
        for &byte in b"BigWorld" {
            expected = (expected ^ byte as u64).wrapping_mul(FNV_PRIME);
        }
        assert_eq!(hash_string(b"BigWorld"), expected);
    }

    #[test]
    fn type_ids_are_stable_unique_and_context_scoped() {
        let a_view_first = scene_view_runtime_id::<TypeA>();
        let a_view_again = scene_view_runtime_id::<TypeA>();
        assert_eq!(a_view_first, a_view_again, "same type answers the same id");
        assert_ne!(
            scene_view_runtime_id::<TypeA>(),
            scene_view_runtime_id::<TypeB>(),
            "distinct types answer distinct ids"
        );
        assert_ne!(
            scene_view_runtime_id::<TypeA>(),
            object_runtime_id::<TypeA>(),
            "the three contexts hand out independent ids"
        );
        assert_ne!(
            object_runtime_id::<TypeA>(),
            object_operation_runtime_id::<TypeA>()
        );
        // 0 is reserved for UNKNOWN: every real id is non-zero.
        assert_ne!(a_view_first, UNKNOWN);
    }

    #[test]
    fn type_id_context_round_trips_and_answers_unknown_out_of_range() {
        let mut context = TypeIDContext::default();
        assert_eq!(context.get_global_id(0), UNKNOWN as u64);
        assert_eq!(context.get_global_id(200), UNKNOWN as u64);

        let g1 = 0x1234_5678_9abc_def0;
        let g2 = 0x0fed_cba9_8765_4321;
        let l1 = context.get_local_id(g1);
        let l1_again = context.get_local_id(g1);
        assert_eq!(l1, l1_again, "mapping is stable");
        assert_eq!(l1, 1, "local ids are 1-based contiguous");
        let l2 = context.get_local_id(g2);
        assert_eq!(l2, 2);
        assert_eq!(context.get_global_id(l1), g1);
        assert_eq!(context.get_global_id(l2), g2);
    }

    #[test]
    fn generate_and_fetch_type_names_round_trip() {
        let id = generate_scene_type_id("some/unique/type/name");
        let again = generate_scene_type_id("some/unique/type/name");
        assert_eq!(id, again);
        assert_eq!(fetch_type_name(id), Some("some/unique/type/name"));
        assert_eq!(fetch_type_name(0), None, "UNKNOWN has no name");
        // A never-registered id answers no name (reference hands back the
        // table's default NULL).
        assert_eq!(fetch_type_name(u64::from(u8::MAX) << 8), None);
    }

    #[test]
    fn hash_combine_matches_the_reference_form() {
        let mut seed = 42u64;
        hash_combine(&mut seed, 7);
        let expected = 42
            ^ 7u64
                .wrapping_add(0xBC6EF372)
                .wrapping_add(42 << 5)
                .wrapping_add(42 >> 3);
        assert_eq!(seed, expected);
    }
}
