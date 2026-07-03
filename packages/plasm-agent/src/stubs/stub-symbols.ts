import type { CapabilityIntrospectionJson, CatalogIntrospectionJson } from "./catalog-introspection.js";
import {
  classifyInvokeShape,
  type CapabilityInvokeShape,
} from "./capability-invoke-shape.js";

function capabilitySearchMethodSegment(cap: CapabilityIntrospectionJson): string {
  const ent = cap.entity.toLowerCase();
  const prefix = `${ent}_`;
  const stripped = cap.name.startsWith(prefix) ? cap.name.slice(prefix.length) : cap.name;
  return stripped.replaceAll("_", "-");
}

function primarySearchCapabilityName(
  catalog: CatalogIntrospectionJson,
  entity: string,
): string | undefined {
  const caps = catalog.capabilities
    .filter((c) => c.entity === entity && c.kind.toLowerCase() === "search")
    .sort((a, b) => a.name.localeCompare(b.name));
  return caps[0]?.name;
}

export interface EntitySymbolBinding {
  entity: string;
  symbol: string;
}

export interface CapabilityBinding {
  capability: string;
  entitySymbol: string;
  methodSymbol?: string;
  methodWire: string;
  invokeShape: CapabilityInvokeShape;
  /** Primary search uses `e#~text`; non-primary uses `e#.search-by-date(...)`. */
  searchSurface?: "tilde" | "named-dot";
  searchMethodSegment?: string;
}

/** Entities with capabilities, stable lexicographic order → `e1`…`eN`. */
export function stubEntityNames(catalog: CatalogIntrospectionJson): string[] {
  return catalog.entities
    .filter((e) => catalog.capabilities.some((c) => c.entity === e.name))
    .map((e) => e.name)
    .sort((a, b) => a.localeCompare(b));
}

/** Deterministic `e#` from catalog entity order (no teaching session / intent). */
export function assignEntitySymbols(entityNames: string[]): Map<string, EntitySymbolBinding> {
  const sorted = [...entityNames].sort((a, b) => a.localeCompare(b));
  const out = new Map<string, EntitySymbolBinding>();
  sorted.forEach((entity, index) => {
    out.set(entity, { entity, symbol: `e${index + 1}` });
  });
  return out;
}

function methodSymbolForCapability(
  cap: CapabilityIntrospectionJson,
  entityCaps: CapabilityIntrospectionJson[],
): string | undefined {
  const shape = classifyInvokeShape(cap);
  if (shape !== "MethodObject" && shape !== "MethodUnion") {
    return undefined;
  }
  const dotted = entityCaps
    .filter((c) => {
      const s = classifyInvokeShape(c);
      return s === "MethodObject" || s === "MethodUnion";
    })
    .sort((a, b) => a.name.localeCompare(b.name));
  const index = dotted.findIndex((c) => c.name === cap.name);
  if (index < 0) return undefined;
  return `m${index + 1}`;
}

/** Deterministic invoke bindings from introspection only (program API, not agent teaching). */
export function assignCapabilityBindings(
  catalog: CatalogIntrospectionJson,
): Map<string, CapabilityBinding> {
  const entityNames = stubEntityNames(catalog);
  const entitySymbols = assignEntitySymbols(entityNames);
  const capsByEntity = new Map<string, CapabilityIntrospectionJson[]>();
  for (const cap of catalog.capabilities) {
    const list = capsByEntity.get(cap.entity) ?? [];
    list.push(cap);
    capsByEntity.set(cap.entity, list);
  }

  const out = new Map<string, CapabilityBinding>();
  for (const cap of catalog.capabilities) {
    const entityBinding = entitySymbols.get(cap.entity);
    if (!entityBinding) continue;
    const entityCaps = capsByEntity.get(cap.entity) ?? [];
    const invokeShape = classifyInvokeShape(cap);
    const primarySearch =
      invokeShape === "SearchText" || invokeShape === "SearchFiltered"
        ? primarySearchCapabilityName(catalog, cap.entity)
        : undefined;
    out.set(cap.name, {
      capability: cap.name,
      entitySymbol: entityBinding.symbol,
      methodSymbol: methodSymbolForCapability(cap, entityCaps),
      methodWire: cap.invoke_wire_name,
      invokeShape,
      searchSurface:
        primarySearch != null
          ? cap.name === primarySearch
            ? "tilde"
            : "named-dot"
          : undefined,
      searchMethodSegment:
        primarySearch != null && cap.name !== primarySearch
          ? capabilitySearchMethodSegment(cap)
          : undefined,
    });
  }
  return out;
}

export function capabilityReturnTypeName(entityName: string): string {
  return entityName.replace(/[^a-zA-Z0-9_]/g, "_");
}
