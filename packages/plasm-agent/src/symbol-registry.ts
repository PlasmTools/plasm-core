/**
 * Agent-global append-only symbol registry (e#/m#/p#/r#).
 * Teaching waves disclose symbols; this mirror supports operator UI and persistence.
 */

export type SymbolKind = "entity" | "method" | "param" | "relation";

export interface SymbolBinding {
  symbol: string;
  kind: SymbolKind;
  entryId: string;
  wire: string;
  entity?: string;
  tombstoned?: boolean;
}

export interface SymbolRegistrySnapshot {
  bindings: SymbolBinding[];
  nextEntity: number;
  nextMethod: number;
  nextParam: number;
  nextRelation: number;
}

export class SymbolRegistry {
  private bindings: SymbolBinding[] = [];
  private counters = { entity: 1, method: 1, param: 1, relation: 1 };

  mint(kind: SymbolKind): string {
    const prefix =
      kind === "entity"
        ? "e"
        : kind === "method"
          ? "m"
          : kind === "param"
            ? "p"
            : "r";
    const key = `${prefix}${this.counters[kind]}` as keyof typeof this.counters;
    this.counters[kind] += 1;
    return `${prefix}${this.counters[kind] - 1}`;
  }

  bind(binding: Omit<SymbolBinding, "symbol"> & { symbol?: string }): SymbolBinding {
    const symbol = binding.symbol ?? this.mint(binding.kind);
    const row: SymbolBinding = { ...binding, symbol };
    this.bindings.push(row);
    return row;
  }

  tombstone(symbol: string): void {
    const row = this.bindings.find((b) => b.symbol === symbol);
    if (row) row.tombstoned = true;
  }

  snapshot(): SymbolRegistrySnapshot {
    return {
      bindings: [...this.bindings],
      nextEntity: this.counters.entity,
      nextMethod: this.counters.method,
      nextParam: this.counters.param,
      nextRelation: this.counters.relation,
    };
  }

  restore(snapshot: SymbolRegistrySnapshot): void {
    this.bindings = [...snapshot.bindings];
    this.counters = {
      entity: snapshot.nextEntity,
      method: snapshot.nextMethod,
      param: snapshot.nextParam,
      relation: snapshot.nextRelation,
    };
  }
}
