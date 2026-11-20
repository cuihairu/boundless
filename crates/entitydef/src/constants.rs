//! `EntityDef::Constants` (`lib/entitydef/constants.hpp`): the fixed resource
//! paths every entity definition lookup goes through.
//!
//! The reference hard-codes these strings; keeping them verbatim is what makes a
//! ported definition loader accept an unmodified BigWorld `res/scripts` tree.

/// Common script tree.
pub const COMMON_PATH: &str = "scripts/common";

/// Server-only common scripts.
pub const SERVER_COMMON_PATH: &str = "scripts/server_common";

/// `<OBJECT_ID> INT32 </OBJECT_ID>` alias file.
pub const ALIASES_FILE: &str = "scripts/entity_defs/alias.xml";

/// Database scripts.
pub const DATABASE_PATH: &str = "scripts/db";

/// `<database>` definition file.
pub const XML_DATABASE_FILE: &str = "scripts/db.xml";

/// Bot scripts.
pub const BOT_SCRIPTS_PATH: &str = "scripts/bot";

/// Entity list file.
pub const ENTITIES_FILE: &str = "scripts/entities.xml";

/// `<Entity>.def` directory.
pub const ENTITIES_DEFS_PATH: &str = "scripts/entity_defs";

/// `<Service>.def` directory.
pub const SERVICES_DEFS_PATH: &str = "scripts/service_defs";

/// Component definition directory.
pub const COMPONENTS_DEFS_PATH: &str = "scripts/component_defs";

/// Base entity component scripts.
pub const ENTITIES_BASE_PATH: &str = "scripts/base";

/// Cell entity component scripts.
pub const ENTITIES_CELL_PATH: &str = "scripts/cell";

/// Client entity component scripts.
pub const ENTITIES_CLIENT_PATH: &str = "scripts/client";

/// Service component scripts.
pub const ENTITIES_SERVICE_PATH: &str = "scripts/service";

/// Editor entity component scripts.
pub const ENTITIES_EDITOR_PATH: &str = "scripts/editor";

/// Client capability table.
pub const ENTITIES_CAPABILITIES_FILE: &str = "scripts/common/capabilities.xml";

/// User data object list file.
pub const USER_DATA_OBJECTS_FILE: &str = "scripts/user_data_objects.xml";

/// User data object definition directory.
pub const USER_DATA_OBJECTS_DEFS_PATH: &str = "scripts/user_data_object_defs";

/// User data object client scripts.
pub const USER_DATA_OBJECTS_CLIENT_PATH: &str = "scripts/client";

/// User data object editor scripts.
pub const USER_DATA_OBJECTS_EDITOR_PATH: &str = "scripts/editor";

/// Marker category table.
pub const MARKER_CATEGORIES_FILE: &str = "scripts/marker_categories.xml";

/// Marker entity definitions.
pub const MARKER_ENTITIES_PATH: &str = "scripts/marker_entities";

/// `.def` lookups for entity types (and services): the reference picks the
/// definitions directory from whether the description is a service.
pub fn defs_dir(is_service: bool) -> &'static str {
    if is_service {
        SERVICES_DEFS_PATH
    } else {
        ENTITIES_DEFS_PATH
    }
}

/// `<Component>.def` lookups.
pub fn component_def_file(component_type: &str) -> String {
    format!("{COMPONENTS_DEFS_PATH}/{component_type}.def")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_match_the_reference_constants() {
        // Pinned against `EntityDef::Constants` (constants.hpp) so a stock
        // BigWorld `res/scripts` tree resolves unchanged.
        assert_eq!(ALIASES_FILE, "scripts/entity_defs/alias.xml");
        assert_eq!(ENTITIES_FILE, "scripts/entities.xml");
        assert_eq!(ENTITIES_DEFS_PATH, "scripts/entity_defs");
        assert_eq!(SERVICES_DEFS_PATH, "scripts/service_defs");
        assert_eq!(COMPONENTS_DEFS_PATH, "scripts/component_defs");
        assert_eq!(ENTITIES_BASE_PATH, "scripts/base");
        assert_eq!(ENTITIES_CELL_PATH, "scripts/cell");
        assert_eq!(ENTITIES_CLIENT_PATH, "scripts/client");
        assert_eq!(ENTITIES_SERVICE_PATH, "scripts/service");
        assert_eq!(XML_DATABASE_FILE, "scripts/db.xml");
        assert_eq!(USER_DATA_OBJECTS_FILE, "scripts/user_data_objects.xml");
        assert_eq!(MARKER_CATEGORIES_FILE, "scripts/marker_categories.xml");
    }

    #[test]
    fn defs_dir_follows_the_service_flag() {
        assert_eq!(defs_dir(false), "scripts/entity_defs");
        assert_eq!(defs_dir(true), "scripts/service_defs");
    }

    #[test]
    fn component_def_paths_include_the_type_name() {
        assert_eq!(
            component_def_file("HealthComponent"),
            "scripts/component_defs/HealthComponent.def"
        );
    }
}
