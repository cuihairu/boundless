/**
 * The EntityDef type system, transcribed from the reference's `FORCE_LINK`
 * registry (`lib/entitydef/data_types.cpp`) plus its alias spellings.
 *
 * Everything here is **case-sensitive whole-string** matching: `findChild` and
 * `sectionName` compare with `strcmp` in the reference, so `<flags>`,
 * `<default>` and `int32` are all invalid.
 */

/** Primitive and composite type names the reference accepts in `<Type>`. */
export const KNOWN_TYPES: readonly string[] = [
  "INT8",
  "INT16",
  "INT32",
  "INT64",
  "UINT8",
  "UINT16",
  "UINT32",
  "UINT64",
  "FLOAT32",
  "FLOAT64",
  "FLOAT",
  "STRING",
  "UNICODE_STRING",
  "BLOB",
  "UDO_REF",
  "VECTOR2",
  "VECTOR3",
  "VECTOR4",
  "PYTHON",
  "MAILBOX",
  "ARRAY",
  "TUPLE",
  "FIXED_DICT",
  "CLASS",
  "USER_TYPE",
];

/** Composites that carry their member types in child sections. */
export const COMPOSITE_TYPES: readonly string[] = ["ARRAY", "TUPLE", "FIXED_DICT", "CLASS"];

/** `FLAGS` spellings, from `s_entityDataFlagMappings`
 * (`lib/entitydef/data_description.cpp`). */
export const KNOWN_FLAGS: readonly string[] = [
  "CELL_PRIVATE",
  "CELL_PUBLIC",
  "OTHER_CLIENTS",
  "OWN_CLIENT",
  "BASE",
  "BASE_AND_CLIENT",
  "CELL_PUBLIC_AND_OWN",
  "ALL_CLIENTS",
  "EDITOR_ONLY",
];

/** The four property names `<Volatile>` recognises; every other child is
 * silently ignored by the reference. */
export const VOLATILE_NAMES: readonly string[] = ["position", "yaw", "pitch", "roll"];

/** `<Distribution>` switches. */
export const DISTRIBUTION_ROLES: readonly string[] = ["Cell", "Base", "Client"];

/** `<Exposed>` spellings; `ALL_CLIENTS` is only legal on cell methods. */
export const EXPOSED_FLAGS: readonly string[] = ["", "OWN_CLIENT", "ALL_CLIENTS"];

/** `<ReplayExposureLevel>` spellings. */
export const REPLAY_EXPOSURE_LEVELS: readonly string[] = [
  "",
  "NONE",
  "OTHER_CLIENTS",
  "ALL_CLIENTS",
];

/** Top-level sections a `.def` may contain, in the reference's read order. */
export const DEF_ROOT_SECTIONS: readonly string[] = [
  "Parent",
  "ClientName",
  "Distribution",
  "Persistent",
  "ExplicitDatabaseID",
  "AppealRadius",
  "LoDLevels",
  "Volatile",
  "Implements",
  "Properties",
  "TempProperties",
  "Components",
  "BaseMethods",
  "Methods",
  "CellMethods",
  "ClientMethods",
];

/** Legal children of an entity property. */
export const PROPERTY_SECTIONS: readonly string[] = [
  "Type",
  "Flags",
  "Persistent",
  "Identifier",
  "Indexed",
  "Default",
  "Editable",
  "ExposedForReplay",
  "DatabaseLength",
  "Widget",
  "SendLatestOnly",
  "IsReliable",
  "VariableLengthHeaderSize",
  "DetailLevel",
];

/** Legal children of a property method. */
export const METHOD_SECTIONS: readonly string[] = [
  "Exposed",
  "ReplayExposureLevel",
  "Args",
  "Arg",
  "ReturnValues",
  "DetailDistance",
];

const knownTypeSet = new Set(KNOWN_TYPES);

/** Whether `name` is a reference type name (not an alias). */
export function isKnownType(name: string): boolean {
  return knownTypeSet.has(name);
}

/** Whether `name` is one of the eight `FLAGS` spellings. */
export function isKnownFlag(name: string): boolean {
  return KNOWN_FLAGS.includes(name);
}
