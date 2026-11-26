//! `SceneObject` and the object-operation dispatch table
//! (`lib/scene/scene_object.{hpp,cpp}`).
//!
//! The reference reinterprets an owned `ObjectType*` into a `uintptr`
//! handle (`SceneObject(ObjectType*, flags)`); the port keeps the
//! caller-supplied `u64` identity and the runtime type id in its place —
//! entity identity comes from the registration site, and the type face
//! (`type`/`isType`/`typeName`) is preserved verbatim. `getAs<T>` cannot
//! rebuild a reference from the handle and is not ported (registered).

use std::collections::HashMap;
use std::hash::Hash;

use crate::scene_object_flags::SceneObjectFlags;
use crate::type_system::{RuntimeTypeID, fetch_type_name, hash_combine, object_runtime_id};

/// `SceneObject` (`scene_object.hpp:16-136`): a handle/type/flags triple.
/// Equality compares handle and type only (the reference `operator==`,
/// hpp:112-116); the hash folds all three (`hash<SceneObject>`,
/// hpp:262-273).
#[derive(Debug, Clone)]
pub struct SceneObject {
    handle: u64,
    type_id: RuntimeTypeID,
    flags: SceneObjectFlags,
}

impl SceneObject {
    /// `SceneObject()` (`scene_object.hpp:21-26`).
    pub fn new() -> Self {
        Self {
            handle: 0,
            type_id: 0,
            flags: SceneObjectFlags::new(),
        }
    }

    /// `SceneObject(ObjectType*, flags)` (`scene_object.hpp:35-41`) with a
    /// caller-supplied identity standing in for the pointer.
    pub fn typed<T: ?Sized + 'static>(handle: u64, flags: SceneObjectFlags) -> Self {
        Self {
            handle,
            type_id: object_runtime_id::<T>(),
            flags,
        }
    }

    /// `SceneObject::typeOf<ObjectType>()` (`scene_object.hpp:43-47`).
    pub fn type_of<T: ?Sized + 'static>() -> RuntimeTypeID {
        object_runtime_id::<T>()
    }

    /// `isValid` is `handle_ != 0` (`scene_object.hpp:118-121`).
    pub fn is_valid(&self) -> bool {
        self.handle != 0
    }

    pub fn handle(&self) -> u64 {
        self.handle
    }

    /// `handle(const ObjectHandle&)` (`scene_object.hpp:93-96`).
    pub fn set_handle(&mut self, handle: u64) {
        self.handle = handle;
    }

    pub fn type_id(&self) -> RuntimeTypeID {
        self.type_id
    }

    /// `type(RuntimeTypeID)` (`scene_object.hpp:71-74`).
    pub fn set_type(&mut self, type_id: RuntimeTypeID) {
        self.type_id = type_id;
    }

    /// `type<ObjectType>()` (`scene_object.hpp:76-80`).
    pub fn set_type_of<T: ?Sized + 'static>(&mut self) {
        self.type_id = object_runtime_id::<T>();
    }

    /// `isType<ObjectType>()` (`scene_object.hpp:82-86`).
    pub fn is_type<T: ?Sized + 'static>(&self) -> bool {
        self.type_id == object_runtime_id::<T>()
    }

    pub fn flags(&self) -> &SceneObjectFlags {
        &self.flags
    }

    pub fn flags_mut(&mut self) -> &mut SceneObjectFlags {
        &mut self.flags
    }

    /// `typeName` (`scene_object.hpp:123-128`): the global id of this
    /// object type resolved through the name table.
    pub fn type_name(&self) -> Option<&'static str> {
        let global = crate::type_system::object_context().get_global_id(self.type_id);
        fetch_type_name(global)
    }
}

impl Default for SceneObject {
    fn default() -> Self {
        Self::new()
    }
}

impl PartialEq for SceneObject {
    /// `operator==` compares handle and type, **not** flags.
    fn eq(&self, other: &Self) -> bool {
        self.handle == other.handle && self.type_id == other.type_id
    }
}

impl Eq for SceneObject {}

impl Hash for SceneObject {
    /// `hash<SceneObject>` (`scene_object.hpp:262-273`): combine handle,
    /// type and flags over a zero seed.
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        let mut seed = 0u64;
        hash_combine(&mut seed, self.handle);
        hash_combine(&mut seed, self.type_id as u64);
        hash_combine(&mut seed, u64::from(self.flags.storage()));
        seed.hash(state);
    }
}

/// `ISceneObjectOperation` (`scene_object.hpp:138-143`): the marker every
/// operation base shares.
pub trait ISceneObjectOperation {}

/// `SceneObjectOperation<TypeHandlerInterface>`
/// (`scene_object.hpp:146-254`): handlers keyed by object runtime type id.
/// The reference allocates handlers itself in `addHandler<T, Impl>()`; the
/// port takes a constructed `Box<H>` from the caller (registered).
pub struct SceneObjectOperation<H: ?Sized> {
    type_handlers: HashMap<RuntimeTypeID, Box<H>>,
}

impl<H: ?Sized> SceneObjectOperation<H> {
    pub fn new() -> Self {
        Self {
            type_handlers: HashMap::new(),
        }
    }

    /// `addHandler<ObjectType>` (hpp:165-175): the id decides the slot,
    /// a re-add replaces (the reference `typeHandlers_[objectType] =
    /// handler`, hpp:247-251).
    pub fn add_handler_for<T: ?Sized + 'static>(&mut self, handler: Box<H>) {
        let type_id = object_runtime_id::<T>();
        self.type_handlers.insert(type_id, handler);
    }

    /// `removeHandler<ObjectType>` (hpp:177-184, 234-245): absent ids are
    /// a no-op.
    pub fn remove_handler_for<T: ?Sized + 'static>(&mut self) -> bool {
        let type_id = object_runtime_id::<T>();
        self.type_handlers.remove(&type_id).is_some()
    }

    /// `getHandler<ObjectType>()` (hpp:186-202).
    pub fn get_handler_for<T: ?Sized + 'static>(&self) -> Option<&H> {
        let type_id = object_runtime_id::<T>();
        self.get_handler_by_id(type_id)
    }

    pub fn get_handler_mut_for<T: ?Sized + 'static>(&mut self) -> Option<&mut H> {
        let type_id = object_runtime_id::<T>();
        self.get_handler_mut_by_id(type_id)
    }

    /// `getHandler(typeID)` (hpp:204-213).
    pub fn get_handler_by_id(&self, type_id: RuntimeTypeID) -> Option<&H> {
        self.type_handlers.get(&type_id).map(|boxed| boxed.as_ref())
    }

    pub fn get_handler_mut_by_id(&mut self, type_id: RuntimeTypeID) -> Option<&mut H> {
        self.type_handlers
            .get_mut(&type_id)
            .map(|boxed| boxed.as_mut())
    }

    /// `isSupported(SceneObject)` (hpp:222-225).
    pub fn is_supported(&self, object: &SceneObject) -> bool {
        self.is_supported_type(object.type_id())
    }

    /// `isSupported(RuntimeTypeID)` (hpp:227-230).
    pub fn is_supported_type(&self, type_id: RuntimeTypeID) -> bool {
        self.type_handlers.contains_key(&type_id)
    }
}

impl<H: ?Sized> Default for SceneObjectOperation<H> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EntityBody;
    struct Projectile;

    #[test]
    fn default_object_is_invalid_and_untyped() {
        let object = SceneObject::new();
        assert!(!object.is_valid());
        assert_eq!(object.handle(), 0);
        assert_eq!(object.type_id(), 0);
    }

    #[test]
    fn typed_objects_carry_the_caller_identity_and_type() {
        let object = SceneObject::typed::<EntityBody>(42, SceneObjectFlags::from_storage(1));
        assert!(object.is_valid());
        assert_eq!(object.handle(), 42);
        assert!(object.is_type::<EntityBody>());
        assert!(!object.is_type::<Projectile>());
        assert_eq!(object.type_id(), SceneObject::type_of::<EntityBody>());
        assert!(object.type_name().is_some(), "the type registered a name");
    }

    #[test]
    fn equality_compares_handle_and_type_but_not_flags() {
        let a = SceneObject::typed::<EntityBody>(7, SceneObjectFlags::new());
        let mut b = SceneObject::typed::<EntityBody>(7, SceneObjectFlags::new());
        assert_eq!(a, b);

        // Same handle+type, different flags: still equal.
        b.flags_mut().set_is_dynamic(true);
        assert_eq!(a, b);

        // Same handle, different type: not equal.
        let other_type = SceneObject::typed::<Projectile>(7, SceneObjectFlags::new());
        assert_ne!(a, other_type);

        // Same type, different handle: not equal.
        let other_handle = SceneObject::typed::<EntityBody>(8, SceneObjectFlags::new());
        assert_ne!(a, other_handle);
    }

    #[test]
    fn type_accessors_follow_the_reference_faces() {
        let mut object = SceneObject::new();
        object.set_handle(9);
        assert!(object.is_valid());
        object.set_type_of::<Projectile>();
        assert!(object.is_type::<Projectile>());
        object.set_type(0);
        assert!(!object.is_type::<Projectile>());
    }

    struct MarkerHandler;

    impl ISceneObjectOperation for MarkerHandler {}

    #[test]
    fn operation_handlers_dispatch_by_object_type() {
        let mut operation: SceneObjectOperation<dyn ISceneObjectOperation> =
            SceneObjectOperation::new();
        assert!(!operation.is_supported_type(1));

        operation.add_handler_for::<EntityBody>(Box::new(MarkerHandler));
        assert!(operation.is_supported_type(SceneObject::type_of::<EntityBody>()));
        let object = SceneObject::typed::<EntityBody>(1, SceneObjectFlags::new());
        assert!(operation.is_supported(&object));
        assert!(operation.get_handler_for::<EntityBody>().is_some());
        assert!(
            operation
                .get_handler_by_id(SceneObject::type_of::<Projectile>())
                .is_none()
        );

        // Re-adding replaces the slot (reference map assignment).
        operation.add_handler_for::<EntityBody>(Box::new(MarkerHandler));
        assert!(
            operation
                .get_handler_by_id(SceneObject::type_of::<EntityBody>())
                .is_some()
        );

        // Absent removes are a no-op.
        assert!(!operation.remove_handler_for::<Projectile>());
        assert!(operation.remove_handler_for::<EntityBody>());
        assert!(!operation.is_supported(&object));
    }

    #[test]
    fn typed_mut_handler_lookups_reach_the_slot() {
        let mut operation: SceneObjectOperation<dyn ISceneObjectOperation> =
            SceneObjectOperation::new();
        operation.add_handler_for::<EntityBody>(Box::new(MarkerHandler));
        assert!(operation.get_handler_mut_for::<EntityBody>().is_some());
        assert!(operation.get_handler_mut_for::<Projectile>().is_none());
    }

    #[test]
    fn default_flag_accessors_and_hash_faces() {
        let object = SceneObject::default();
        assert!(!object.is_valid());

        let mut a = SceneObject::typed::<EntityBody>(3, SceneObjectFlags::new());
        a.flags_mut().set_is_dynamic(true);
        assert!(a.flags().is_dynamic());

        // The hash folds all three fields, so differing flags answer
        // different hashes: operator== may call two objects equal while a
        // hashed collection will not find one under the other (reference
        // behavior — hash<SceneObject> folds the flags, operator== does
        // not).
        let b = SceneObject::typed::<EntityBody>(3, SceneObjectFlags::new());
        let mut set = std::collections::HashSet::new();
        set.insert(b);
        assert!(!set.contains(&a), "differing flags hash differently");
        set.insert(a.clone());
        assert!(set.contains(&a));
    }
}
