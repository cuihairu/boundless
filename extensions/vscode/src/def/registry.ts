/**
 * `scripts/entities.xml` and `scripts/entity_defs/alias.xml` readers.
 *
 * `entities.xml` is the entity type registry (`ClientServerEntities` plus
 * `ServerOnlyEntities`); `alias.xml` maps custom type names to complete type
 * specs. The reference registers aliases into a `std::map` at first use
 * (`DataType::initAliases`), so a later duplicate name is silently dropped
 * and aliases may only reference aliases defined earlier in the file.
 */

import { DataSection, findChild, parseXml } from "../datasection/xmlSection";

export interface EntitiesXml {
  clientServerEntities: string[];
  serverOnlyEntities: string[];
  /** Missing `<ServerOnlyEntities>` is a WARNING in the reference. */
  serverOnlyMissing: boolean;
  /** Missing `<ClientServerEntities>` is a NOTICE in the reference. */
  clientServerMissing: boolean;
}

export function parseEntitiesXml(text: string): EntitiesXml {
  const root = parseXml("root", text);
  const clientServer = findChild(root, "ClientServerEntities");
  const serverOnly = findChild(root, "ServerOnlyEntities");
  return {
    clientServerEntities: clientServer ? clientServer.children.map((c) => c.name) : [],
    serverOnlyEntities: serverOnly ? serverOnly.children.map((c) => c.name) : [],
    clientServerMissing: !clientServer,
    serverOnlyMissing: !serverOnly,
  };
}

/** The alias table: a name → type-spec section map. */
export class AliasRegistry {
  private readonly byName = new Map<string, DataSection>();

  static parse(text: string): AliasRegistry {
    const registry = new AliasRegistry();
    const root = parseXml("root", text);
    for (const child of root.children) {
      // std::map::insert drops duplicates silently.
      if (!registry.byName.has(child.name)) {
        registry.byName.set(child.name, child);
      }
    }
    return registry;
  }

  has(name: string): boolean {
    return this.byName.has(name);
  }

  /** The alias spec section, or `undefined`. */
  get(name: string): DataSection | undefined {
    return this.byName.get(name);
  }

  names(): string[] {
    return [...this.byName.keys()];
  }
}
