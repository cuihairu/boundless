//! The collision dispatch table (`lib/scene/collision_operation.{hpp,cpp}`):
//! per-object-type sweep handlers driven by the scene's collision walk.
//!
//! The reference's two `doCollide` overloads arrive as distinct method
//! names (`do_collide_ray` for the `Vector3` source, `do_collide_prism` for
//! the `WorldTriangle` source).

use crate::scene_object::{SceneObject, SceneObjectOperation};
use foundation::math::Vector3;
use physics::{CollisionState, SweepParams, WorldTriangle};

/// `ICollisionTypeHandler` (`collision_operation.hpp:21-43`): each face
/// answers whether the sweep was stopped early (true) or ran to completion
/// (false).
pub trait ICollisionTypeHandler {
    fn do_collide_ray(
        &self,
        object: &SceneObject,
        source: &Vector3,
        extent: &Vector3,
        sp: &SweepParams,
        state: &mut CollisionState,
    ) -> bool;

    fn do_collide_prism(
        &self,
        object: &SceneObject,
        source: &WorldTriangle,
        extent: &Vector3,
        sp: &SweepParams,
        state: &mut CollisionState,
    ) -> bool;
}

/// `CollisionOperation : SceneObjectOperation<ICollisionTypeHandler>`
/// (`collision_operation.hpp:45-56`): missing handlers answer false
/// (`collision_operation.cpp:13-24` on both faces).
#[derive(Default)]
pub struct CollisionOperation {
    base: SceneObjectOperation<dyn ICollisionTypeHandler>,
}

impl CollisionOperation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn base(&self) -> &SceneObjectOperation<dyn ICollisionTypeHandler> {
        &self.base
    }

    pub fn base_mut(&mut self) -> &mut SceneObjectOperation<dyn ICollisionTypeHandler> {
        &mut self.base
    }

    /// `collide(object, Vector3, ...)` (`collision_operation.cpp:13-24`).
    pub fn collide_ray(
        &self,
        object: &SceneObject,
        source: &Vector3,
        extent: &Vector3,
        sp: &SweepParams,
        state: &mut CollisionState,
    ) -> bool {
        match self.base.get_handler_by_id(object.type_id()) {
            Some(handler) => handler.do_collide_ray(object, source, extent, sp, state),
            None => false,
        }
    }

    /// `collide(object, WorldTriangle, ...)` (`collision_operation.cpp:26-37`).
    pub fn collide_prism(
        &self,
        object: &SceneObject,
        source: &WorldTriangle,
        extent: &Vector3,
        sp: &SweepParams,
        state: &mut CollisionState,
    ) -> bool {
        match self.base.get_handler_by_id(object.type_id()) {
            Some(handler) => handler.do_collide_prism(object, source, extent, sp, state),
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene_object_flags::SceneObjectFlags;
    use physics::CollisionCallback;
    use std::sync::Mutex;

    struct Crate;
    struct Ghost;

    fn object<T: ?Sized + 'static>(handle: u64) -> SceneObject {
        SceneObject::typed::<T>(handle, SceneObjectFlags::new())
    }

    static RAY_HITS: Mutex<Vec<u64>> = Mutex::new(Vec::new());
    static PRISM_HITS: Mutex<Vec<u64>> = Mutex::new(Vec::new());

    /// Records which face ran for which handle, ray always stops the
    /// sweep, prism always lets it run on.
    struct SharedHandler;

    impl ICollisionTypeHandler for SharedHandler {
        fn do_collide_ray(
            &self,
            object: &SceneObject,
            _source: &Vector3,
            _extent: &Vector3,
            _sp: &SweepParams,
            _state: &mut CollisionState,
        ) -> bool {
            RAY_HITS.lock().unwrap().push(object.handle());
            true
        }

        fn do_collide_prism(
            &self,
            object: &SceneObject,
            _source: &WorldTriangle,
            _extent: &Vector3,
            _sp: &SweepParams,
            _state: &mut CollisionState,
        ) -> bool {
            PRISM_HITS.lock().unwrap().push(object.handle());
            false
        }
    }

    struct NopCallback;
    impl CollisionCallback for NopCallback {}

    #[test]
    fn collision_dispatch_answers_by_object_type_and_missing_handlers_pass() {
        let mut operation = CollisionOperation::new();
        operation
            .base_mut()
            .add_handler_for::<Crate>(Box::new(SharedHandler));

        let source = Vector3::ZERO;
        let extent = Vector3::new(0.0, 0.0, 10.0);
        let triangle = WorldTriangle::new(
            Vector3::new(-1.0, 0.0, 0.0),
            Vector3::new(1.0, 0.0, 0.0),
            Vector3::new(0.0, 1.0, 0.0),
            0,
        );
        let sp = SweepParams::new(&physics::SweepShape::Ray(source), extent);
        let mut cb = NopCallback;
        let mut state = CollisionState::new(&mut cb);

        // No handler for Ghost: both faces pass through untouched.
        assert!(!operation.collide_ray(&object::<Ghost>(1), &source, &extent, &sp, &mut state));
        assert!(!operation.collide_prism(&object::<Ghost>(1), &triangle, &extent, &sp, &mut state));
        assert!(RAY_HITS.lock().unwrap().is_empty());
        assert!(PRISM_HITS.lock().unwrap().is_empty());

        // Crate: the ray face runs and reports the stop.
        assert!(operation.collide_ray(&object::<Crate>(2), &source, &extent, &sp, &mut state));
        assert_eq!(*RAY_HITS.lock().unwrap(), [2]);

        // The prism face reports no stop.
        assert!(!operation.collide_prism(&object::<Crate>(3), &triangle, &extent, &sp, &mut state));
        assert_eq!(*PRISM_HITS.lock().unwrap(), [3]);

        // Support queries mirror the registration.
        assert!(operation.base().is_supported(&object::<Crate>(2)));
        assert!(!operation.base().is_supported(&object::<Ghost>(1)));

        // Removing the handler restores pass-through.
        assert!(operation.base_mut().remove_handler_for::<Crate>());
        assert!(!operation.collide_ray(&object::<Crate>(4), &source, &extent, &sp, &mut state));
        assert_eq!(RAY_HITS.lock().unwrap().len(), 1);
    }
}
