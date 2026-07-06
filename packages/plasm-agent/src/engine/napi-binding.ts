/**
 * Plasm engine binding — in-process NAPI (`@plasm_lang/engine`) with stub fallback.
 */

import { createRequire } from "node:module";
import type { LoadedCatalog } from "../catalog/loader.js";
import { toNapiHostTransport } from "./host-transport-bridge.js";

const nodeRequire = createRequire(import.meta.url);

export interface TeachingExposureResult {
  tsv: string;
  deltaRefs: string[];
  executeSessionRef?: string;
}

export interface DryRunResult {
  planCommitRef: string;
  summary: string;
  compJson?: unknown;
}

export interface ResolvedPlanPayload {
  /** application/vnd.plasm.resolved-plan+json */
  planJson: unknown;
  catalogCgsHash: string;
}

/** Host-injected outbound HTTP transport (Connect bearer, etc.). */
export type HostTransportRequest = {
  method: string;
  url: string;
  headers?: Record<string, string>;
  body?: string;
  entryId?: string;
};

export type HostTransportResponse = {
  status: number;
  body: string;
  nextUrl?: string;
};

export type HostTransportFn = (request: HostTransportRequest) => Promise<HostTransportResponse>;

export interface PlasmEngine {
  loadCatalog(catalog: LoadedCatalog): Promise<void>;
  synthesizeTeaching(
    intent: string,
    seeds: Array<{ api: string; entity: string }>,
  ): Promise<TeachingExposureResult>;
  dryRun(program: string, executeSessionRef?: string): Promise<DryRunResult>;
  discover(intent: string): Promise<{ markdown: string }>;
  runPlan(planCommitRef: string): Promise<{ ok: boolean; message: string; rowsJson?: string; metaJson?: string }>;
  runPlanLive?(
    planCommitRef: string,
    transport: HostTransportFn,
  ): Promise<{ ok: boolean; message: string; rowsJson?: string; metaJson?: string }>;
  run(resolved: ResolvedPlanPayload, transport: HostTransportFn): Promise<unknown>;
  introspectCatalog(entryId: string): Promise<string>;
}

type NativePlasmEngine = {
  loadCatalog(catalogDir: string): {
    entryId: string;
    catalogCgsHash: string;
  };
  exposeSeeds(
    intent: string,
    seeds: Array<{ api: string; entity: string }>,
  ): {
    tsv: string;
    deltaRefs: string[];
  };
  dryRun(program: string): {
    planCommitRef: string;
    summary: string;
    compJson: string;
  };
  discover(intent: string): {
    markdown: string;
  };
  runPlan(planCommitRef: string): {
    ok: boolean;
    message: string;
    rowsJson?: string;
    metaJson?: string;
  };
  runPlanLive(
    planCommitRef: string,
    transport: (request: HostTransportRequest) => Promise<HostTransportResponse>,
  ): Promise<{
    ok: boolean;
    message: string;
    rowsJson?: string;
    metaJson?: string;
  }>;
  introspectCatalog(entryId: string): string;
};

type NativeConstructor = new () => NativePlasmEngine;

function loadNativeConstructor(): NativeConstructor | null {
  try {
    const mod = nodeRequire("@plasm_lang/engine") as { PlasmEngine: NativeConstructor };
    return mod.PlasmEngine ?? null;
  } catch {
    return null;
  }
}

/** NAPI-backed engine (`@plasm_lang/engine`). Build with `npm run build` in `packages/plasm-engine`. */
export class NapiPlasmEngine implements PlasmEngine {
  private readonly native: NativePlasmEngine;
  private loaded: LoadedCatalog[] = [];

  constructor(NativeEngine: NativeConstructor = loadNativeConstructor()!) {
    if (!NativeEngine) {
      throw new Error(
        "@plasm_lang/engine native binding not found — run `npm run build` in packages/plasm-engine",
      );
    }
    this.native = new NativeEngine();
  }

  async loadCatalog(catalog: LoadedCatalog): Promise<void> {
    const info = this.native.loadCatalog(catalog.rootDir);
    this.loaded.push({
      ...catalog,
      manifest: {
        ...catalog.manifest,
        entryId: info.entryId,
        cgsHash: info.catalogCgsHash,
      },
    });
  }

  async synthesizeTeaching(
    intent: string,
    seeds: Array<{ api: string; entity: string }>,
  ): Promise<TeachingExposureResult> {
    void this.loaded;
    const result = this.native.exposeSeeds(intent, seeds);
    return {
      tsv: result.tsv,
      deltaRefs: result.deltaRefs,
    };
  }

  async dryRun(program: string, executeSessionRef?: string): Promise<DryRunResult> {
    void executeSessionRef;
    const result = this.native.dryRun(program);
    let compJson: unknown;
    try {
      compJson = JSON.parse(result.compJson) as unknown;
    } catch {
      compJson = result.compJson;
    }
    return {
      planCommitRef: result.planCommitRef,
      summary: result.summary,
      compJson,
    };
  }

  async discover(intent: string): Promise<{ markdown: string }> {
    return this.native.discover(intent);
  }

  async runPlan(planCommitRef: string): Promise<{ ok: boolean; message: string; rowsJson?: string }> {
    return this.native.runPlan(planCommitRef);
  }

  async runPlanLive(
    planCommitRef: string,
    transport: HostTransportFn,
  ): Promise<{ ok: boolean; message: string; rowsJson?: string; metaJson?: string }> {
    return this.native.runPlanLive(planCommitRef, toNapiHostTransport(transport));
  }

  async run(_resolved: ResolvedPlanPayload, transport: HostTransportFn): Promise<unknown> {
    void _resolved;
    throw new Error(
      "NapiPlasmEngine.run(resolvedPlan): pass planCommitRef via runPlanLive(pcN, transport) instead",
    );
  }

  async introspectCatalog(entryId: string): Promise<string> {
    return this.native.introspectCatalog(entryId);
  }
}

/** Stub until `@plasm_lang/engine` is built or on unsupported platforms. */
export class StubPlasmEngine implements PlasmEngine {
  private loaded: LoadedCatalog[] = [];

  async loadCatalog(catalog: LoadedCatalog): Promise<void> {
    this.loaded.push(catalog);
  }

  async synthesizeTeaching(
    intent: string,
    seeds: Array<{ api: string; entity: string }>,
  ): Promise<TeachingExposureResult> {
    void intent;
    return {
      tsv: `# teaching TSV placeholder\n# seeds: ${JSON.stringify(seeds)}`,
      deltaRefs: [],
    };
  }

  async dryRun(program: string, executeSessionRef?: string): Promise<DryRunResult> {
    void executeSessionRef;
    return {
      planCommitRef: "pc0",
      summary: `[stub dry-run] ${program.slice(0, 80)}`,
    };
  }

  async discover(intent: string): Promise<{ markdown: string }> {
    return {
      markdown: `# Discovery stub\n\nIntent: ${intent}\n\nLoad @plasm_lang/engine native binding for real discovery TSV.`,
    };
  }

  async runPlan(planCommitRef: string): Promise<{ ok: boolean; message: string; rowsJson?: string }> {
    return {
      ok: false,
      message: `[stub] plan ${planCommitRef} — wire NAPI binding`,
    };
  }

  async runPlanLive(
    planCommitRef: string,
    _transport: HostTransportFn,
  ): Promise<{ ok: boolean; message: string; rowsJson?: string }> {
    return {
      ok: false,
      message: `[stub] live run ${planCommitRef} — build @plasm_lang/engine`,
    };
  }

  async run(_resolved: ResolvedPlanPayload, _transport: HostTransportFn): Promise<unknown> {
    throw new Error("StubPlasmEngine.run: wire NAPI binding or plasm-server sidecar");
  }

  async introspectCatalog(_entryId: string): Promise<string> {
    throw new Error("StubPlasmEngine.introspectCatalog: build @plasm_lang/engine");
  }
}

export function createEngine(mode: "napi" | "stub" = "napi"): PlasmEngine {
  if (mode === "stub") {
    return new StubPlasmEngine();
  }
  const Native = loadNativeConstructor();
  if (Native) {
    return new NapiPlasmEngine(Native);
  }
  return new StubPlasmEngine();
}

export function isNativeEngineAvailable(): boolean {
  return loadNativeConstructor() !== null;
}
