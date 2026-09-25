import type { CapabilityIntrospectionJson, CatalogIntrospectionJson } from "./catalog-introspection.js";
import {
  classifyInvokeShape,
  type CapabilityInvokeShape,
} from "./capability-invoke-shape.js";

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

}

/** Entities with capabilities, stable lexicographic order → `e1`…`eN`. */
export function stubEntityNames(catalog: CatalogIntrospectionJson): string[] {
  return catalog.entities
    .filter((e) => catalog.capabilities.some((c) => c.entity === e.name))
    .map((e) => e.name)
    .sort((a, b) => Buffer.compare(Buffer.from(a), Buffer.from(b)));
}

/** Deterministic `e#` from catalog entity order (no teaching session / intent). */
export function assignEntitySymbols(entityNames: string[]): Map<string, EntitySymbolBinding> {
  const sorted = [...entityNames].sort((a, b) => Buffer.compare(Buffer.from(a), Buffer.from(b)));
  const out = new Map<string, EntitySymbolBinding>();
  sorted.forEach((entity, index) => {
    out.set(entity, { entity, symbol: `e${index + 1}` });
  });
  return out;
}

/** Bindings are allocated by the compiler, never guessed from method order. */
export function assignCapabilityBindings(catalog: CatalogIntrospectionJson): Map<string, CapabilityBinding> {
  const out = new Map<string, CapabilityBinding>();
  for (const cap of catalog.capabilities) {
    if (!cap.python) throw new Error("Catalog introspection lacks Python bindings; rebuild the native engine");
    out.set(cap.name, { capability: cap.name, entitySymbol: cap.python.entity_symbol,
      methodSymbol: cap.python.method, methodWire: cap.invoke_wire_name, invokeShape: classifyInvokeShape(cap) });
  }
  return out;
}

export function capabilityReturnTypeName(entityName: string): string {
  return entityName.replace(/[^a-zA-Z0-9_]/g, "_");
}
