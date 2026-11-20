/**
 * The `.def` document model.
 *
 * The entity name never appears in the file: the reference derives it from the
 * file name (`scripts/entity_defs/<Name>.def`, `lib/entitydef/constants.hpp`),
 * so `parseEntityDef` takes it as an argument. Everything else is read exactly
 * where `EntityDescription::parse` / `parseInterface` / `parseProperties` /
 * `parseMethods` read it.
 */

import { DataSection, findChild, openSection, parseXml, readString } from "../datasection/xmlSection";

/** Which component a method is declared for. */
export type MethodComponent = "base" | "cell" | "client" | "service";

export interface MethodInfo {
  /** The element tag name; the reference uses `sectionName()` as the name. */
  name: string;
  component: MethodComponent;
  /** The method element itself. */
  node: DataSection;
  /** Type-spec sections, in declaration order. */
  argSpecs: DataSection[];
  /** True when the method used the legacy `<Arg>` spelling. */
  oldStyleArgs: boolean;
}

export interface EntityDef {
  /**
   * The entity name. The reference derives it from the file name
   * (`scripts/entity_defs/<Name>.def`) and passes it to `DataDescription::parse`
   * as `parentName`, so reference messages read `<Name>.<property>`.
   */
  name: string;
  root: DataSection;
  /** `<Parent>` — the only inheritance mechanism in this fork. */
  parent: string | undefined;
  /** `<ClientName>` (deprecated). */
  clientName: string | undefined;
  /** Services read `<Methods>` instead of `<BaseMethods>`. */
  isService: boolean;
  /** Children of `<Properties>`, in document order. */
  properties: readonly DataSection[];
  /** Children of `<TempProperties>`. */
  tempProperties: readonly DataSection[];
  methods: readonly MethodInfo[];
  distribution: Readonly<Record<string, boolean>>;
  /** The raw `<Volatile>` section, when present. */
  volatile: DataSection | undefined;
}

/** The `<Volatile>` recognises only these four children. */
export function volatilePriorities(def: EntityDef): Record<string, number | undefined> {
  const out: Record<string, number | undefined> = {};
  if (!def.volatile) return out;
  for (const name of ["position", "yaw", "pitch", "roll"]) {
    const child = findChild(def.volatile, name);
    if (!child) continue;
    const value = prefixFloat(child.value);
    // The reference maps -1 to `ALWAYS` (FLT_MAX) and squares the rest.
    out[name] = value === -1 ? Number.POSITIVE_INFINITY : (value ?? 0) * (value ?? 0);
  }
  return out;
}

/** Parses a `.def` document; `name` comes from the file name. */
export function parseEntityDef(
  text: string,
  name: string,
  isService = false,
): EntityDef {
  const root = parseXml("root", text);

  const distribution: Record<string, boolean> = {};
  const dist = findChild(root, "Distribution");
  if (dist) {
    for (const child of dist.children) distribution[child.name] = child.value.toLowerCase() !== "false";
  }

  const methods: MethodInfo[] = [];
  const methodSections: Array<[string, MethodComponent]> = isService
    ? [["Methods", "service"]]
    : [
        ["BaseMethods", "base"],
        ["CellMethods", "cell"],
        ["ClientMethods", "client"],
      ];

  for (const [tag, component] of methodSections) {
    const section = findChild(root, tag);
    if (!section) continue;
    for (const node of section.children) {
      methods.push(collectMethod(node, component));
    }
  }

  return {
    name,
    root,
    parent: readString(root, "Parent") || undefined,
    clientName: readString(root, "ClientName") || undefined,
    isService,
    properties: childrenOf(root, "Properties"),
    tempProperties: childrenOf(root, "TempProperties"),
    methods,
    distribution,
    volatile: findChild(root, "Volatile"),
  };
}

function childrenOf(node: DataSection, tag: string): DataSection[] {
  const section = findChild(node, tag);
  return section ? [...section.children] : [];
}

/** The reference tries `<Args>` first and falls back to legacy `<Arg>`
 * children of the method element. */
function collectMethod(node: DataSection, component: MethodComponent): MethodInfo {
  const argsSection = findChild(node, "Args");
  const oldStyle = !argsSection;
  const specSource = argsSection ?? node;
  const argSpecs = oldStyle
    ? specSource.children.filter((c) => c.name === "Arg")
    : [...specSource.children];
  return { name: node.name, component, node, argSpecs, oldStyleArgs: oldStyle };
}

/** `readFloat`-equivalent prefix parse. */
function prefixFloat(value: string): number | undefined {
  const m = /^[+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][+-]?\d+)?/.exec(value.trim());
  return m ? Number(m[0]) : undefined;
}

/** Convenience: the resolved `<Type>` section of a property. */
export function typeSection(property: DataSection): DataSection | undefined {
  return findChild(property, "Type");
}

/** Convenience: the type text, resolving alias wrappers left to right. */
export function typeSpec(property: DataSection): { name: string; node: DataSection | undefined; raw: string } {
  const node = typeSection(property);
  if (!node) return { name: "", node: undefined, raw: "" };
  return { name: node.value, node, raw: openSection(property, "Type")?.value ?? "" };
}
