//! The intersect-query context (`lib/scene/scene_intersect_context.{hpp,cpp}`):
//! per-query flags the providers consult before walking their trees.
//!
//! The reference holds a `const Scene&`; the port holds the scene identity
//! token (registered with the provider delegation).

use crate::SceneToken;

pub const INCLUDE_DYNAMIC_OBJECTS: u32 = 1 << 0;
pub const INCLUDE_STATIC_OBJECTS: u32 = 1 << 1;
pub const ONLY_SHADOW_CASTERS: u32 = 1 << 2;

/// `SceneIntersectContext` (`scene_intersect_context.hpp:14-37`).
#[derive(Debug, Clone, Copy)]
pub struct SceneIntersectContext {
    flags: u32,
    scene: Option<SceneToken>,
}

impl SceneIntersectContext {
    /// `SceneIntersectContext(scene)` (`scene_intersect_context.cpp:8-11`):
    /// both object families included by default.
    pub fn new(scene: Option<SceneToken>) -> Self {
        Self {
            flags: INCLUDE_DYNAMIC_OBJECTS | INCLUDE_STATIC_OBJECTS,
            scene,
        }
    }

    pub fn scene(&self) -> Option<SceneToken> {
        self.scene
    }

    fn set(&mut self, mask: u32, include: bool) {
        self.flags &= !mask;
        self.flags |= if include { mask } else { 0 };
    }

    /// `onlyShadowCasters` (`scene_intersect_context.cpp:13-15`).
    pub fn only_shadow_casters(&self) -> bool {
        (self.flags & ONLY_SHADOW_CASTERS) != 0
    }

    pub fn set_only_shadow_casters(&mut self, include: bool) {
        self.set(ONLY_SHADOW_CASTERS, include);
    }

    /// `includeDynamicObjects` (`scene_intersect_context.cpp:22-24`).
    pub fn include_dynamic_objects(&self) -> bool {
        (self.flags & INCLUDE_DYNAMIC_OBJECTS) != 0
    }

    pub fn set_include_dynamic_objects(&mut self, include: bool) {
        self.set(INCLUDE_DYNAMIC_OBJECTS, include);
    }

    /// `includeStaticObjects` (`scene_intersect_context.cpp:31-33`).
    pub fn include_static_objects(&self) -> bool {
        (self.flags & INCLUDE_STATIC_OBJECTS) != 0
    }

    pub fn set_include_static_objects(&mut self, include: bool) {
        self.set(INCLUDE_STATIC_OBJECTS, include);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_defaults_to_both_families_and_round_trips_flags() {
        let mut context = SceneIntersectContext::new(Some(7));
        assert_eq!(context.scene(), Some(7));
        assert!(context.include_dynamic_objects());
        assert!(context.include_static_objects());
        assert!(!context.only_shadow_casters());

        context.set_include_dynamic_objects(false);
        assert!(!context.include_dynamic_objects());
        assert!(context.include_static_objects());

        context.set_include_static_objects(false);
        context.set_only_shadow_casters(true);
        assert!(context.only_shadow_casters());
        assert!(!context.include_dynamic_objects());
        assert!(!context.include_static_objects());

        // Re-enabling restores the bit without touching the others.
        context.set_include_dynamic_objects(true);
        assert!(context.include_dynamic_objects());
        assert!(!context.include_static_objects());
        assert!(context.only_shadow_casters());

        let unbound = SceneIntersectContext::new(None);
        assert!(unbound.scene().is_none());
    }
}
