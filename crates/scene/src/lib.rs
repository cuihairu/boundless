//! BW-10-02-04 the scene delegation layer and entity mapping — a port of
//! `lib/scene` (the slice the server consumes):
//!
//! - the runtime type system with its three id contexts
//!   (`scene_type_system.{hpp,cpp}`) and the `bw_hash` primitives it
//!   builds on,
//! - `SceneObject` — the typed handle an entity registers as its scene
//!   presence — and `SceneObjectFlags` (`scene_object.{hpp,cpp}`,
//!   `scene_object_flags.{hpp,cpp}`),
//! - the provider/view delegation: `Scene` aggregates providers, lazily
//!   creates views and object operations (`scene.{hpp,cpp}`,
//!   `scene_provider.{hpp,cpp}`),
//! - the tick face: `TickSceneView`, its listeners and the type-dispatched
//!   `TickOperation` (`tick_scene_view.{hpp,cpp}`).
//!
//! Ownership adaptations are registered at each site: the reference passes
//! raw `Scene*`/`ProviderInterface*` pointers around a shared graph; the
//! port hands out `Rc` faces and a scene identity token in their place,
//! keeping the reference's asserts (add-once, null-then-set scene) and
//! dispatch order verbatim.

pub mod provider;
pub mod scene_object;
pub mod scene_object_flags;
pub mod tick_view;
pub mod type_system;

pub use provider::{
    ISceneView, ProviderSceneRef, Scene, SceneProvider, SceneToken, SceneViewRef, expose_tick_view,
    tick_provider_view_id,
};
pub use scene_object::{SceneObject, SceneObjectOperation};
pub use scene_object_flags::{
    IS_CASTING_DYNAMIC_SHADOW, IS_CASTING_STATIC_SHADOW, IS_DYNAMIC_OBJECT, IS_REFLECTION_VISIBLE,
    IS_TERRAIN_OBJECT, SceneObjectFlags, StorageType as SceneObjectFlagsStorageType,
};
pub use tick_view::{
    ITickOperationTypeHandler, ITickSceneListener, ITickSceneProvider, TickOperation, TickSceneView,
};
pub use type_system::{
    GlobalUniqueTypeID, RuntimeTypeID, TypeIDContext, UNKNOWN, fetch_type_name,
    generate_scene_type_id, globally_unique_type_id, hash_combine, hash_string,
    object_operation_runtime_id, object_runtime_id, scene_view_runtime_id,
};
