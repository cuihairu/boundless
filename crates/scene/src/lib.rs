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
//!   `TickOperation` (`tick_scene_view.{hpp,cpp}`),
//! - the query projections: the collision sweep walk
//!   (`collision_scene_view.{hpp,cpp}` + `collision_operation.{hpp,cpp}`)
//!   and the visibility intersect walk (`intersect_scene_view.{hpp,cpp}`
//!   + `intersection_set.{hpp,cpp}` + `scene_intersect_context.{hpp,cpp}`),
//! - the object-change event face (`object_change_scene_view.{hpp,cpp}`
//!   + `scene_event_view.{hpp,cpp}`).
//!
//! Ownership adaptations are registered at each site: the reference passes
//! raw `Scene*`/`ProviderInterface*` pointers around a shared graph; the
//! port hands out `Rc` faces and a scene identity token in their place,
//! keeping the reference's asserts (add-once, null-then-set scene) and
//! dispatch order verbatim. One structural adaptation: the reference's
//! providers pull views/operations from the `Scene*` inside `onSetScene`;
//! the port cannot re-enter the scene while a provider's `RefCell` is
//! mutably borrowed, so `Scene::add_provider` materializes the
//! [`provider::SceneBoundFaces`] and pushes them to the provider through
//! `on_scene_bound`.

pub mod collision_operation;
pub mod collision_scene_view;
pub mod intersect_scene_view;
pub mod intersection_set;
pub mod object_change_scene_view;
pub mod provider;
pub mod scene_intersect_context;
pub mod scene_object;
pub mod scene_object_flags;
pub mod tick_view;
pub mod type_system;

pub use collision_operation::{CollisionOperation, ICollisionTypeHandler};
pub use collision_scene_view::{CollisionSceneView, ICollisionSceneViewProvider};
pub use intersect_scene_view::{IIntersectSceneViewProvider, IntersectSceneView};
pub use intersection_set::IntersectionSet;
pub use object_change_scene_view::{ObjectChangeArgs, ObjectChangeDelegate, ObjectChangeSceneView};
pub use provider::{
    ISceneView, ProviderSceneRef, Scene, SceneBoundFaces, SceneProvider, SceneToken, SceneViewRef,
    collision_provider_view_id, expose_collision_view, expose_intersect_view, expose_tick_view,
    intersect_provider_view_id, tick_provider_view_id,
};
pub use scene_intersect_context::{
    INCLUDE_DYNAMIC_OBJECTS, INCLUDE_STATIC_OBJECTS, ONLY_SHADOW_CASTERS, SceneIntersectContext,
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
