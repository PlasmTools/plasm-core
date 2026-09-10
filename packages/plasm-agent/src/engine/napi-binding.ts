/**
 * Plasm engine binding — in-process NAPI (`@plasm_lang/engine`).
 */

import { createRequire } from "node:module";
import type { LoadedCatalog } from "../catalog/loader.js";
import { routingPacketSchema, type RoutingPacket } from "./routing.js";
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
  /** MCP-aligned: clean 0-write plans execute inline from `plasm`. */
  fusedCleanRead?: boolean;
}

export interface ResolvedPlanPayload {
  /** application/vnd.plasm.resolved-plan+json */
  planJson: unknown;
  catalogCgsHash: string;
}

/** Host-injected outbound HTTP transport (Connect bearer, etc.). */
export type HostTransportRequest = {
  rejectRedirects: boolean;
  /** Resolve configured injection or reject before dispatch when true. */
  requireHostAuth: boolean;
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
  activateDiscovery(deploymentId: string, bindingsJson: string): Promise<string>;
  routeIntent(intent: string, logicalSessionId?: string): Promise<RoutingPacket>;
  runPlan(planCommitRef: string, logicalSessionId?: string): Promise<{ ok: boolean; message: string; rowsJson?: string; metaJson?: string; artifactsJson?: string }>;
  runPlanLive?(
    planCommitRef: string,
    transport: HostTransportFn,
    logicalSessionId?: string,
  ): Promise<{ ok: boolean; message: string; rowsJson?: string; metaJson?: string; artifactsJson?: string }>;
  run(resolved: ResolvedPlanPayload, transport: HostTransportFn): Promise<unknown>;
  introspectCatalog(entryId: string): Promise<string>;
}

type NativePlasmEngine = {
  loadCatalog(catalogDir: string): Promise<{
    entryId: string;
    catalogCgsHash: string;
  }>;
  exposeSeeds(
    intent: string,
    seeds: Array<{ api: string; entity: string }>,
  ): Promise<{
    tsv: string;
    deltaRefs: string[];
  }>;
  dryRun(program: string, logicalSessionId?: string): Promise<{
    planCommitRef: string;
    summary: string;
    compJson: string;
    fusedCleanRead?: boolean;
  }>;
  activateDiscovery(deploymentId: string, bindingsJson: string): Promise<string>;
  routeIntent(intent: string, logicalSessionId?: string): Promise<string>;
  runPlan(planCommitRef: string, logicalSessionId?: string): Promise<{
    ok: boolean;
    message: string;
    rowsJson?: string;
    metaJson?: string; artifactsJson?: string;
  }>;
  runPlanLive(
    planCommitRef: string,
    transport: (request: HostTransportRequest) => Promise<HostTransportResponse>,
    logicalSessionId?: string,
  ): Promise<{
    ok: boolean;
    message: string;
    rowsJson?: string;
    metaJson?: string; artifactsJson?: string;
  }>;
  introspectCatalog(entryId: string): Promise<string>;
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
    const info = await this.native.loadCatalog(catalog.manifestPath);
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
    const result = await this.native.exposeSeeds(intent, seeds);
    return {
      tsv: result.tsv,
      deltaRefs: result.deltaRefs,
    };
  }

  async dryRun(program: string, executeSessionRef?: string): Promise<DryRunResult> {
    const result = await this.native.dryRun(program, executeSessionRef);
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
      fusedCleanRead: result.fusedCleanRead === true,
    };
  }

  async activateDiscovery(deploymentId: string, bindingsJson: string): Promise<string> {
    return this.native.activateDiscovery(deploymentId, bindingsJson);
  }

  async routeIntent(intent: string, logicalSessionId?: string): Promise<RoutingPacket> {
    const raw = await this.native.routeIntent(intent, logicalSessionId);
    return routingPacketSchema.parse(JSON.parse(raw));
  }

  async runPlan(planCommitRef: string, logicalSessionId?: string): Promise<{ ok: boolean; message: string; rowsJson?: string }> {
    return this.native.runPlan(planCommitRef, logicalSessionId);
  }

  async runPlanLive(
    planCommitRef: string,
    transport: HostTransportFn,
    logicalSessionId?: string,
  ): Promise<{ ok: boolean; message: string; rowsJson?: string; metaJson?: string; artifactsJson?: string }> {
    return this.native.runPlanLive(planCommitRef, toNapiHostTransport(transport), logicalSessionId);
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

export function createEngine(): PlasmEngine {
  return new NapiPlasmEngine();
}

export function isNativeEngineAvailable(): boolean {
  return loadNativeConstructor() !== null;
}
