/**
 * EntityDef validation, reproducing the fatal/warning rules of the reference
 * `EntityDescriptionMap::parse` and `tools/process_defs`.
 *
 * Every rule below quotes the reference message verbatim and cites where the
 * reference raises it. Rules the reference treats as non-fatal are reported as
 * warnings so the accepted-file set stays identical (`process_defs` exits
 * non-zero only on a fatal rule).
 */

import { DataSection, findChild, readBool } from "../datasection/xmlSection";
import { EntityDef, MethodInfo, volatilePriorities } from "./defFile";
import { AliasRegistry } from "./registry";
import { isKnownFlag, isKnownType, VOLATILE_NAMES } from "./types";

export type Severity = "error" | "warning";

/** A diagnostic anchored at absolute offsets in the source text. */
export interface DefDiagnostic {
  severity: Severity;
  message: string;
  offset: number;
  length: number;
  /** Stable rule id, used by tests and the problem matcher. */
  rule: string;
}

/** Everything a single `.def` needs to be checked against. */
export interface CheckContext {
  aliases: AliasRegistry;
  /** Whether a script file exists for the given role. */
  hasScript?: (role: "base" | "cell" | "client" | "service") => boolean;
}

/** Runs every rule over one parsed `.def`. */
export function checkEntityDef(def: EntityDef, ctx: CheckContext): DefDiagnostic[] {
  const out: DefDiagnostic[] = [];
  const aliases = ctx.aliases;

  out.push(...checkDistribution(def, ctx));
  out.push(...checkVolatile(def));
  out.push(...checkProperties(def, aliases));
  out.push(...checkMethods(def, aliases));

  return out;
}

/** Distribution/script cross-checks (`EntityDescription::parse`,
 * `lib/entitydef/entity_description.cpp:289-340`).
 *
 * Whether a role is available comes from `HasScriptOrTagDistributionDecider`
 * (`entity_description.cpp:72`): a `<Distribution>` tag wins, otherwise the
 * reference probes `scripts/cell|base|client/<Name>.py` for existence.
 */
function checkDistribution(def: EntityDef, ctx: CheckContext): DefDiagnostic[] {
  const out: DefDiagnostic[] = [];
  const has = ctx.hasScript;
  if (!has) return out;

  const wanted = (role: "Cell" | "Base" | "Client"): boolean => {
    const explicit = def.distribution[role];
    if (explicit !== undefined) return explicit;
    // A service keeps its base-side script under scripts/service_defs/.
    const scriptRole =
      role === "Base" && def.isService
        ? "service"
        : (role.toLowerCase() as "cell" | "base" | "client");
    return has(scriptRole);
  };

  const firstMethod = (component: MethodInfo["component"]): MethodInfo | undefined =>
    def.methods.find((m) => m.component === component);

  if (!wanted("Cell")) {
    // A cell property already sinks the streaming of cell data, so the reference
    // reports the property rather than the method.
    const cellProperty = def.properties.find((p) => isCellDataFlag(flagOf(p)));
    if (cellProperty) {
      out.push(
        diag(
          "warning",
          `Entity '${def.name}' does not have a cell script but has cell property '${cellProperty.name}'.`,
          cellProperty,
          "distribution.cell-property",
        ),
      );
    } else {
      const first = firstMethod("cell");
      if (first) {
        out.push(
          diag(
            "warning",
            `Entitiy '${def.name}' does not have a cell script but has cell method '${first.name}'`,
            first.node,
            "distribution.cell-script",
          ),
        );
      }
    }
  }

  if (!wanted("Base")) {
    if (def.isService) {
      out.push(
        diag("error", `Service '${def.name}' does not have a service script.`, def.root, "distribution.service-script"),
      );
    } else {
      const first = firstMethod("base");
      if (first) {
        out.push(
          diag(
            "warning",
            `Entity '${def.name}' does not have a base script but has base method '${first.name}'`,
            first.node,
            "distribution.base-script",
          ),
        );
      }
    }
  }

  const firstClient = firstMethod("client");
  if (!wanted("Client") && firstClient) {
    out.push(
      diag(
        "error",
        `Entity '${def.name}' does not have a client script but has client methods`,
        firstClient.node,
        "distribution.client-script",
      ),
    );
  }

  return out;
}

/** `DataDescription::isCellData` (`data_description.ipp:114`): the flag sets
 * `DATA_GHOSTED`. */
function isCellDataFlag(flags: string): boolean {
  return (
    flags === "CELL_PUBLIC" ||
    flags === "OTHER_CLIENTS" ||
    flags === "CELL_PUBLIC_AND_OWN" ||
    flags === "ALL_CLIENTS"
  );
}

/** The raw `<Flags>` text of a property. */
function flagOf(property: DataSection): string {
  return findChild(property, "Flags")?.value ?? "";
}

/** `VolatileInfo::parse` (`lib/entitydef/volatile_info.cpp`): unknown children
 * are ignored, and yaw ≥ pitch ≥ roll must hold. */
function checkVolatile(def: EntityDef): DefDiagnostic[] {
  const out: DefDiagnostic[] = [];
  if (!def.volatile) return out;

  const priorities = volatilePriorities(def);
  const yaw = priorities.yaw ?? Number.NEGATIVE_INFINITY;
  const pitch = priorities.pitch ?? Number.NEGATIVE_INFINITY;
  const roll = priorities.roll ?? Number.NEGATIVE_INFINITY;

  if (!(yaw >= pitch && pitch >= roll)) {
    const condition = yaw < pitch ? "yaw < pitch" : "pitch < roll";
    out.push(
      diag(
        "error",
        `VolatileInfo::parse: Invalid direction priorities (${condition}). The values for yaw, pitch and roll must be descending.`,
        def.volatile,
        "volatile.priority-order",
      ),
    );
  }

  for (const child of def.volatile.children) {
    if (!VOLATILE_NAMES.includes(child.name)) {
      // Documented as silent in the reference; surfaced as a hint-level
      // warning because a typo like <postion/> silently disables updates.
      out.push(
        diag(
          "warning",
          `Volatile child '${child.name}' is not one of position, yaw, pitch, roll and will be ignored`,
          child,
          "volatile.unknown-child",
        ),
      );
    }
  }

  return out;
}

function checkProperties(def: EntityDef, aliases: AliasRegistry): DefDiagnostic[] {
  const out: DefDiagnostic[] = [];
  const labels = lodLabels(def);
  for (const property of def.properties) {
    out.push(...checkProperty(property, def.name, aliases, labels));
  }
  out.push(...checkLoDLevels(def));
  return out;
}

/** `DataLoDLevels::addLevels` registers one level per `<LoDLevels>` child,
 * capped at `MAX_DATA_LOD_LEVELS`. */
const MAX_DATA_LOD_LEVELS = 6;

function checkLoDLevels(def: EntityDef): DefDiagnostic[] {
  const section = findChild(def.root, "LoDLevels");
  if (!section || section.children.length <= MAX_DATA_LOD_LEVELS) return [];
  return [
    diag(
      "error",
      `DataLoDLevels::addLevels: Only allowed ${MAX_DATA_LOD_LEVELS} levels.`,
      section,
      "lod.level-count",
    ),
  ];
}

/** The level labels declared under `<LoDLevels>`: each child names its label
 * in a `<label>` child. */
function lodLabels(def: EntityDef): Set<string> {
  const labels = new Set<string>();
  const section = findChild(def.root, "LoDLevels");
  if (!section) return labels;
  for (const level of section.children) {
    const label = findChild(level, "label");
    if (label) labels.add(label.value);
  }
  return labels;
}

/** `DataDescription::parse` (`lib/entitydef/data_description.cpp:169-331`).
 * `entityName` is what the reference passes as `parentName`, so the messages
 * read `<Entity>.<property>`. */
function checkProperty(
  property: DataSection,
  entityName: string,
  aliases: AliasRegistry,
  labels: Set<string>,
): DefDiagnostic[] {
  const out: DefDiagnostic[] = [];
  const name = property.name;
  const member = `${entityName}.${name}`;

  const typeNode = findChild(property, "Type");
  const resolved = resolveType(property, aliases, 0);

  if (!typeNode || resolved === null) {
    // `DataType::buildDataType` reports the missing meta type first, then
    // `DataDescription::parse` reports the consequence.
    out.push(
      diag(
        "error",
        `DataType::buildDataType: Could not find MetaDataType '${typeNode?.value ?? ""}'`,
        typeNode ?? property,
        "type.metadatatype-missing",
      ),
    );
    out.push(
      diag(
        "error",
        `DataDescription::parse: Unable to find data type '${typeNode?.value ?? ""}' for ${member}`,
        typeNode ?? property,
        "property.type-missing",
      ),
    );
    return out;
  }

  out.push(...checkCompositeSpec(typeNode));

  const flagsNode = findChild(property, "Flags");
  const flagsText = flagsNode ? flagsNode.value : "";
  if (!isKnownFlag(flagsText)) {
    out.push(
      diag(
        "error",
        `DataDescription::parse: Invalid Flags section '${flagsText}' for ${name}`,
        flagsNode ?? property,
        "property.flags-invalid",
      ),
    );
  }

  const persistent = readBool(property, "Persistent", false);
  const identifier = readBool(property, "Identifier", false);
  const indexed = readBool(property, "Indexed", identifier);
  if (identifier && !persistent) {
    out.push(
      diag(
        "error",
        `DataDescription::parse: ${name} must be persistent to be indexed`,
        findChild(property, "Identifier") ?? property,
        "property.identifier-not-persistent",
      ),
    );
  }
  if (indexed && !persistent && !identifier) {
    out.push(
      diag(
        "error",
        `DataDescription::parse: ${name} must be persistent to be indexed`,
        findChild(property, "Indexed") ?? property,
        "property.indexed-not-persistent",
      ),
    );
  }

  // Client-server properties must not carry MAILBOX (fatal) or PYTHON
  // (warning) members. `EntityDescription::parseProperties` raises these.
  if (isClientServerFlag(flagsText)) {
    const leaks = collectTypes(resolved);
    for (const leak of leaks) {
      if (leak.name === "PYTHON") {
        out.push(
          diag(
            "warning",
            `${member} is a client-server property that has a PYTHON member (potential security hole)`,
            leak.node,
            "property.client-server-python",
          ),
        );
      } else if (leak.name === "MAILBOX") {
        out.push(
          diag(
            "error",
            `${member} is a client-server property that has a MAILBOX member (type is not able to be sent to or from client)`,
            leak.node,
            "property.client-server-mailbox",
          ),
        );
      }
    }
  }

  // The reference only resolves the detail level for other-client data
  // (`isOtherClientData`, i.e. `DATA_OTHER_CLIENT`: OTHER_CLIENTS or
  // ALL_CLIENTS), against the labels declared in <LoDLevels>.
  const detailLevel = findChild(property, "DetailLevel");
  if (
    detailLevel &&
    isOtherClientFlag(flagsText) &&
    !labels.has(detailLevel.value)
  ) {
    out.push(
      diag(
        "error",
        `EntityDescription::parseProperties: Invalid detail level for ${name}.`,
        detailLevel,
        "property.detail-level",
      ),
    );
  }

  return out;
}

/** Structural rules of the type spec itself: `ARRAY`/`TUPLE` need `<of>`,
 * `USER_TYPE` needs a dotted `<implementedBy>`. */
function checkCompositeSpec(typeNode: DataSection): DefDiagnostic[] {
  const out: DefDiagnostic[] = [];
  const head = typeNode.value.split(/\s+/)[0];

  if (head === "ARRAY" || head === "TUPLE") {
    const of = findChild(typeNode, "of");
    if (!of) {
      out.push(
        diag(
          "error",
          `SequenceMetaDataType::getType: Unable to create sequence of '${head}'`,
          typeNode,
          "property.sequence-no-of",
        ),
      );
    }
    return out;
  }

  if (head === "USER_TYPE") {
    const implementedBy = findChild(typeNode, "implementedBy");
    if (!implementedBy) {
      out.push(
        diag(
          "error",
          "UserMetaDataType::getType: implementedBy is not specified.",
          typeNode,
          "property.user-type-implemented-by",
        ),
      );
    } else if (!implementedBy.value.includes(".")) {
      out.push(
        diag(
          "error",
          `UserMetaDataType::getType: Invalid implementedBy ${implementedBy.value} - must contain a '.'`,
          implementedBy,
          "property.user-type-implemented-by",
        ),
      );
    }
  }

  return out;
}

/** `DataDescription::isClientServerData`
 * (`lib/entitydef/data_description.ipp:144`): the flag sets
 * `DATA_OTHER_CLIENT`, `DATA_OWN_CLIENT` or `DATA_REPLAY`. Replayed data is
 * only ever client-server data, so this is exactly the five spellings that set
 * one of the first two bits in `s_entityDataFlagMappings`. */
function isClientServerFlag(flags: string): boolean {
  return (
    flags === "ALL_CLIENTS" ||
    flags === "OTHER_CLIENTS" ||
    flags === "OWN_CLIENT" ||
    flags === "BASE_AND_CLIENT" ||
    flags === "CELL_PUBLIC_AND_OWN"
  );
}

/** `DataDescription::isOtherClientData`
 * (`lib/entitydef/data_description.ipp:92`): the flag sets `DATA_OTHER_CLIENT`. */
function isOtherClientFlag(flags: string): boolean {
  return flags === "OTHER_CLIENTS" || flags === "ALL_CLIENTS";
}


function checkMethods(def: EntityDef, aliases: AliasRegistry): DefDiagnostic[] {
  const out: DefDiagnostic[] = [];
  const seen = new Map<string, string>();

  for (const method of def.methods) {
    out.push(...checkMethod(def, method, aliases));

    const signature = `${method.component}:${method.argSpecs.map((a) => a.value).join(",")}`;
    const key = `${method.component}:${method.name}`;
    const previous = seen.get(key);
    if (previous !== undefined && previous !== signature) {
      out.push(
        diag(
          "error",
          `EntityMethodDescriptions::init: Method '${method.name}' already defined with a different signature`,
          method.node,
          "method.duplicate-signature",
        ),
      );
    }
    seen.set(key, signature);
  }

  return out;
}

/** `MethodDescription::parse` (`lib/entitydef/method_description.cpp`). */
function checkMethod(
  def: EntityDef,
  method: MethodInfo,
  aliases: AliasRegistry,
): DefDiagnostic[] {
  const out: DefDiagnostic[] = [];

  const exposed = findChild(method.node, "Exposed");
  if (exposed) {
    if (method.component === "client") {
      out.push(
        diag(
          "error",
          `MethodDescription::parse: Unable to use <Exposed> tag in client method ${method.name}.`,
          exposed,
          "method.exposed-in-client",
        ),
      );
    } else if (!isValidExposed(exposed.value, method.component)) {
      const allowed = method.component === "cell" ? " ALL_CLIENTS | OWN_CLIENT " : " OWN_CLIENT ";
      out.push(
        diag(
          "error",
          `MethodDescription::parse: Could not recognise ${exposed.value} flag in <Exposed> tag in ${method.name}. Use <Exposed> [${allowed}] </Exposed>`,
          exposed,
          "method.exposed-invalid",
        ),
      );
    }
  }

  const replay = findChild(method.node, "ReplayExposureLevel");
  if (replay && !["NONE", "OTHER_CLIENTS", "ALL_CLIENTS"].includes(replay.value)) {
    out.push(
      diag(
        "error",
        `MethodDescription::parse: Could not recognise ${replay.value} flag in <ReplayExposureLevel> tag in ${method.name}. Use <ReplayExposureLevel> NONE | OTHER_CLIENTS | ALL_CLIENTS </ReplayExposureLevel>`,
        replay,
        "method.replay-exposure-level",
      ),
    );
  }

  // Each arg spec must resolve to a type; the generic `<Arg>` spelling may
  // repeat, named args may not.
  const argNames = new Set<string>();
  method.argSpecs.forEach((spec, index) => {
    if (resolveTypeSpec(spec, aliases, 0) === null) {
      out.push(
        diag(
          "error",
          `MethodArgs::parse: Return type #${index} is invalid`,
          spec,
          "method.arg-type",
        ),
      );
    }
    if (!method.oldStyleArgs) {
      if (argNames.has(spec.name) && spec.name !== "Arg") {
        out.push(
          diag(
            "error",
            `MethodArgs::parse: Duplicate argument name '${spec.name}'`,
            spec,
            "method.arg-duplicate",
          ),
        );
      }
      argNames.add(spec.name);
    }
  });

  // Client-safety of the arguments. `EntityMethodDescriptions::init` marks
  // every client method as exposed ("all client methods are exposed",
  // entity_method_descriptions.cpp:56) and then runs
  // `checkExposedForClientSafety` over the exposed methods only.
  const exposedToClients =
    method.component === "client" || exposed !== undefined;
  const kind = method.component === "client" ? "a client" : "an Exposed";

  if (exposedToClients) {
    for (const spec of method.argSpecs) {
      const resolved = resolveTypeSpec(spec, aliases, 0);
      if (resolved === null) continue;
      for (const member of collectTypes(resolved)) {
        if (member.name === "PYTHON") {
          out.push(
            diag(
              "warning",
              `${def.name}.${method.name} is ${kind} method that takes a PYTHON arg (potential security hole)`,
              member.node,
              "method.client-python-arg",
            ),
          );
        } else if (member.name === "MAILBOX") {
          out.push(
            diag(
              "error",
              `${def.name}.${method.name} is ${kind} method that takes a MAILBOX arg (type is not able to be sent to or from client)`,
              member.node,
              "method.exposed-mailbox-arg",
            ),
          );
        }
      }
    }
  }

  // Client methods are the only ones with `<ReturnValues>` restrictions; the
  // reference only checks the arg side, so nothing else is emitted here.
  return out;
}

function isValidExposed(value: string, component: MethodInfo["component"]): boolean {
  if (value === "") return true;
  if (value === "OWN_CLIENT") return true;
  if (value === "ALL_CLIENTS") return component === "cell";
  return false;
}

/** Resolves a property's `<Type>` through the alias table.
 * `null` means the chain does not terminate at a known type. */
export function resolveType(
  property: DataSection,
  aliases: AliasRegistry,
  depth: number,
): DataSection | null {
  const typeNode = findChild(property, "Type");
  if (!typeNode) return null;
  return resolveTypeSpec(typeNode, aliases, depth);
}

/** Resolves a type-spec section: a known type name, or an alias whose own spec
 * resolves. Aliases may only reference aliases defined earlier, so the caller
 * passes the already-built registry. */
export function resolveTypeSpec(
  spec: DataSection,
  aliases: AliasRegistry,
  depth: number,
): DataSection | null {
  if (depth > 32) return null;

  // Composite and primitive specs are spelled `TYPE <of>…</of>` and so resolve
  // on their first word; anything else must be an alias name.
  const first = spec.value.split(/\s+/)[0];
  if (isKnownType(first)) return spec;

  const alias = aliases.get(spec.value);
  return alias ? resolveTypeSpec(alias, aliases, depth + 1) : null;
}


/** One named type reachable from a type spec, with the section it came from. */
export interface LeakedType {
  name: string;
  node: DataSection;
}

/** Every named type reachable from a resolved type spec, itself included. */
export function collectTypes(spec: DataSection, depth = 0): LeakedType[] {
  if (depth > 32) return [];
  const first = spec.value.split(/\s+/)[0];
  const out: LeakedType[] = [];

  if (isKnownType(first)) {
    out.push({ name: first, node: spec });
  }

  const of = findChild(spec, "of");
  if (of) out.push(...collectTypes({ ...of, value: of.value || of.children[0]?.value || "" }, depth + 1));

  for (const child of spec.children) {
    if (child === of) continue;
    // A CLASS spec nests member specs under <Properties>, one per member.
    if (child.name === "Properties") {
      for (const member of child.children) {
        const memberType = findChild(member, "Type");
        if (memberType) out.push(...collectTypes(memberType, depth + 1));
      }
      continue;
    }
    if (isKnownType(child.value.split(/\s+/)[0])) {
      out.push(...collectTypes(child, depth + 1));
    }
  }

  return out;
}

function diag(
  severity: Severity,
  message: string,
  node: DataSection,
  rule: string,
): DefDiagnostic {
  const offset = node.nameOffset >= 0 ? node.nameOffset : 0;
  const length = node.nameLength > 0 ? node.nameLength : 1;
  return { severity, message, offset, length, rule };
}

