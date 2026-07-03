import path from "node:path";
import { createHash, randomUUID } from "node:crypto";

import type { LoadedCatalog } from "../catalog/loader.js";
import { FilesystemCatalogLoader } from "../catalog/loader.js";
import {
  createEngine,
  type HostTransportFn,
  type PlasmEngine,
} from "../engine/napi-binding.js";
import { createDefaultHostTransport } from "../engine/host-transport.js";
import { mintLogicalSessionId } from "../runtime/logical-session.js";
import { SessionManager, type AgentSessionState } from "../session-state.js";
import {
  formatPlasmContextMarkdown,
  formatPlasmDryRunMarkdown,
  formatPlasmRunMarkdown,
} from "../tools/format.js";
import { LocalArchiveStore } from "../archive/index.js";
import { createArchiveStore } from "../archive/resolve-backend.js";
import type { ProdArchiveStore } from "../archive/prod-archive-store.js";
import { computeRunId } from "../archive/run-id.js";
import { activeTraceId, plasmSpans } from "../telemetry/plasm-spans.js";
import { PlasmSpanAttributes } from "../instrumentation.js";
import type { AuthoringContext } from "../authoring/context.js";
import type { HookRunner } from "../authoring/hook-runner.js";
import type { AgentWorkflowWorldDefinition } from "../define-agent.js";
import { createAgentStateStore } from "../state/define-state.js";

export type AgentArchiveStore = LocalArchiveStore | ProdArchiveStore;

export interface AgentRuntimeConfig {
  agentRoot: string;
  tenantScope?: string;
  engine?: PlasmEngine;
  /** Outbound HTTP for live `plasm_run`. Defaults to fetch + env bearer + Connect. Set `null` to validate-only. */
  hostTransport?: HostTransportFn | null;
  /** When false, skip local archive writes. Default true. */
  archiveEnabled?: boolean;
  archive?: AgentArchiveStore | null;
  /** Workflow/state world — selects fs vs KV vs Postgres session mirror. */
  stateWorld?: AgentWorkflowWorldDefinition;
  hookRunner?: HookRunner;
  getAuthoringContext?: () => AuthoringContext;
}

export interface DiscoverInput {
  intent: string;
}

export interface PlasmContextInput {
  intent: string;
  seeds: Array<{ api: string; entity: string }>;
  rankedCapabilities?: string[] | null;
}

export interface PlasmPlanInput {
  logicalSessionRef: string;
  program: string;
  reasoning?: string;
}

export interface PlasmRunInput {
  logicalSessionRef: string;
  runRef: string;
  reasoning?: string;
}

function seedKey(seed: { api: string; entity: string }): string {
  return `${seed.api}:${seed.entity}`;
}

function mergeSeeds(
  existing: Array<{ api: string; entity: string }>,
  incoming: Array<{ api: string; entity: string }>,
): Array<{ api: string; entity: string }> {
  const seen = new Set(existing.map(seedKey));
  const out = [...existing];
  for (const seed of incoming) {
    const key = seedKey(seed);
    if (seen.has(key)) continue;
    seen.add(key);
    out.push(seed);
  }
  return out;
}

function planArchiveEnabled(): boolean {
  const flag = process.env.PLASM_WRITE_PLAN_ARCHIVE?.trim();
  if (flag === "0" || flag === "false") return false;
  return true;
}

interface RunPlasmMeta {
  steps?: Array<{ request_fingerprints?: string[] }>;
  request_fingerprints?: string[];
}

function parseRunMeta(metaJson?: string): RunPlasmMeta | undefined {
  if (!metaJson?.trim()) return undefined;
  try {
    const parsed = JSON.parse(metaJson) as { plasm?: RunPlasmMeta };
    return parsed.plasm;
  } catch {
    return undefined;
  }
}

function collectRequestFingerprints(meta?: RunPlasmMeta): string[] {
  if (!meta) return [];
  const fromSteps = (meta.steps ?? []).flatMap((step) => step.request_fingerprints ?? []);
  const direct = meta.request_fingerprints ?? [];
  return [...new Set([...fromSteps, ...direct])];
}

function parseRowsJson(rowsJson?: string): unknown[] | undefined {
  if (!rowsJson?.trim()) return undefined;
  try {
    const parsed = JSON.parse(rowsJson) as unknown;
    return Array.isArray(parsed) ? parsed : [parsed];
  } catch {
    return undefined;
  }
}

export class AgentRuntime {
  readonly engine: PlasmEngine;
  readonly sessionManager: SessionManager;
  readonly archive: AgentArchiveStore | null;
  readonly hostTransport: HostTransportFn | null;
  private readonly agentRoot: string;
  private readonly archiveEnabled: boolean;
  private readonly hookRunner?: HookRunner;
  private readonly getAuthoringContext?: () => AuthoringContext;
  private loadedCatalogs: LoadedCatalog[] = [];

  constructor(config: AgentRuntimeConfig) {
    this.agentRoot = path.resolve(config.agentRoot);
    this.engine = config.engine ?? createEngine();
    this.hostTransport =
      config.hostTransport === null
        ? null
        : (config.hostTransport ?? createDefaultHostTransport({ useConnect: true }));
    const tenantScope = config.tenantScope ?? "local";
    const stateStore = createAgentStateStore({
      agentRoot: this.agentRoot,
      tenantScope,
      world: config.stateWorld,
    });
    this.sessionManager = new SessionManager(stateStore, tenantScope);
    this.archiveEnabled = config.archiveEnabled ?? true;
    this.hookRunner = config.hookRunner;
    this.getAuthoringContext = config.getAuthoringContext;
    if (config.archive === null) {
      this.archive = null;
    } else if (config.archive) {
      this.archive = config.archive;
    } else if (this.archiveEnabled) {
      this.archive = createArchiveStore(this.agentRoot, { world: config.stateWorld });
    } else {
      this.archive = null;
    }
  }

  async bootstrap(): Promise<LoadedCatalog[]> {
    if (this.archive) {
      await this.archive.bootstrap();
    }
    const loader = new FilesystemCatalogLoader();
    const catalogs = await loader.discover(this.agentRoot);
    for (const catalog of catalogs) {
      await this.engine.loadCatalog(catalog);
    }
    this.loadedCatalogs = catalogs;
    return catalogs;
  }

  listCatalogs(): LoadedCatalog[] {
    return [...this.loadedCatalogs];
  }

  async discoverCapabilities(input: DiscoverInput): Promise<string> {
    return plasmSpans.toolDiscover({ intent: input.intent.trim() }, async (span) => {
      const intent = input.intent.trim();
      if (!intent) {
        throw new Error("discover_capabilities `intent` must be a non-empty string");
      }
      const started = Date.now();
      const result = await this.engine.discover(intent);
      await this.recordToolTrace("tool", "discover_capabilities", started, {
        intent,
        trace_id: activeTraceId() ?? span.spanContext().traceId,
      });
      return result.markdown;
    });
  }

  async openOrExtendSession(intent: string): Promise<AgentSessionState> {
    const trimmed = intent.trim();
    let session = await this.sessionManager.get(trimmed);
    if (session) return session;
    const ids = mintLogicalSessionId(this.sessionManager.tenant(), trimmed);
    return this.sessionManager.getOrCreate(
      trimmed,
      ids.logicalSessionRef,
      ids.logicalSessionId,
    );
  }

  async plasmContext(input: PlasmContextInput): Promise<string> {
    const intent = input.intent.trim();
    const entryId = input.seeds[0]?.api;
    const catalogCgsHash = entryId ? this.catalogHashForEntry(entryId) : undefined;

    return plasmSpans.toolContext(
      {
        intent,
        entryId,
        catalogCgsHash,
      },
      async (span) => {
        if (!intent) throw new Error("plasm_context requires `intent`");
        if (!input.seeds.length) throw new Error("plasm_context requires non-empty `seeds`");

        void input.rankedCapabilities;

        let session = await this.openOrExtendSession(intent);
        const before = new Set(session.seeds.map(seedKey));
        const merged = mergeSeeds(session.seeds, input.seeds);
        const hasNew = merged.length > before.size;

        const started = Date.now();
        const exposure = await this.engine.synthesizeTeaching(intent, input.seeds);
        const reused = !hasNew && !exposure.tsv.trim();

        if (exposure.tsv.trim()) {
          session.teachingTsv = session.teachingTsv
            ? `${session.teachingTsv.trim()}\n\n${exposure.tsv.trim()}`
            : exposure.tsv.trim();
          session.waves.push({
            entryId: input.seeds[0]?.api ?? "unknown",
            entities: input.seeds.map((s) => s.entity),
            tsv: exposure.tsv,
            at: new Date().toISOString(),
          });
        }
        session.seeds = merged;
        await this.sessionManager.update(session);

        span.setAttribute("plasm.logical_session_ref", session.logicalSessionRef);
        await this.recordToolTrace("tool", "plasm_context", started, {
          intent,
          logical_session_ref: session.logicalSessionRef,
          entry_id: entryId,
          trace_id: activeTraceId() ?? span.spanContext().traceId,
        });

        return formatPlasmContextMarkdown(session.logicalSessionRef, exposure.tsv, reused);
      },
    );
  }

  async plasm(input: PlasmPlanInput): Promise<string> {
    const session = await this.requireSessionByRef(input.logicalSessionRef);
    void input.reasoning;
    const entryId = session.seeds[0]?.api;
    const catalogCgsHash = entryId ? this.catalogHashForEntry(entryId) : undefined;

    return plasmSpans.dryRun(
      {
        intent: session.intent,
        logicalSessionRef: session.logicalSessionRef,
        sessionId: session.logicalSessionId,
        entryId,
        catalogCgsHash,
      },
      async (span) => {
        const started = Date.now();
        const dry = await this.engine.dryRun(input.program);
        session.planCommits.push({
          ref: dry.planCommitRef,
          program: input.program,
          at: new Date().toISOString(),
        });
        await this.sessionManager.update(session);

        span.setAttribute(PlasmSpanAttributes.RUN_REF, dry.planCommitRef);
        if (catalogCgsHash) {
          span.setAttribute("plasm.catalog_cgs_hash", catalogCgsHash);
        }

        if (this.archive && planArchiveEnabled() && catalogCgsHash) {
          await this.archive.writePlanArchive({
            plan_commit_ref: dry.planCommitRef,
            program: input.program,
            catalog_cgs_hash: catalogCgsHash,
            entry_id: entryId,
            logical_session_ref: session.logicalSessionRef,
            intent: session.intent,
            comp_json: dry.compJson,
            archived_at: new Date().toISOString(),
          });
        }

        await this.recordToolTrace("plasm", "plasm.dry_run", started, {
          intent: session.intent,
          logical_session_ref: session.logicalSessionRef,
          plan_commit_ref: dry.planCommitRef,
          catalog_cgs_hash: catalogCgsHash,
          trace_id: activeTraceId() ?? span.spanContext().traceId,
        });

        await this.emitHook("plan:commit", {
          intent: session.intent,
          planCommitRef: dry.planCommitRef,
          runRef: dry.planCommitRef,
          program: input.program,
          logicalSessionRef: session.logicalSessionRef,
        });

        return formatPlasmDryRunMarkdown(dry.summary, dry.planCommitRef);
      },
    );
  }

  async plasmRun(input: PlasmRunInput): Promise<string> {
    const session = await this.requireSessionByRef(input.logicalSessionRef);
    void input.reasoning;
    const entryId = session.seeds[0]?.api;
    const catalogCgsHash = entryId ? this.catalogHashForEntry(entryId) : undefined;
    const planCommit = session.planCommits.find((pc) => pc.ref === input.runRef);

    return plasmSpans.liveRun(
      {
        intent: session.intent,
        logicalSessionRef: session.logicalSessionRef,
        sessionId: session.logicalSessionId,
        runRef: input.runRef,
        entryId,
        catalogCgsHash,
      },
      async (span) => {
        const started = Date.now();
        const result =
          this.hostTransport && typeof this.engine.runPlanLive === "function"
            ? await this.engine.runPlanLive(input.runRef, this.hostTransport)
            : await this.engine.runPlan(input.runRef);
        const program = planCommit?.program ?? "";
        const runMeta = parseRunMeta(result.metaJson);
        const requestFingerprints = collectRequestFingerprints(runMeta);
        const runId =
          catalogCgsHash && program
            ? computeRunId({
                catalogCgsHash,
                planCommitRef: input.runRef,
                program,
                entryId,
                requestFingerprints,
              })
            : `pr${createHash("sha256").update(randomUUID()).digest("hex")}`;

        span.setAttribute("plasm.run_id", runId);
        span.setAttribute(PlasmSpanAttributes.RUN_REF, input.runRef);
        if (catalogCgsHash) {
          span.setAttribute("plasm.catalog_cgs_hash", catalogCgsHash);
        }

        if (this.archive) {
          await this.archive.writeRunSnapshot({
            run_id: runId,
            plan_commit_ref: input.runRef,
            catalog_cgs_hash: catalogCgsHash ?? "unknown",
            entry_id: entryId,
            logical_session_ref: session.logicalSessionRef,
            intent: session.intent,
            ok: result.ok,
            message: result.message,
            results: parseRowsJson(result.rowsJson),
            _meta: {
              plasm: {
                steps: runMeta?.steps ?? [],
                request_fingerprints: requestFingerprints,
              },
            },
            archived_at: new Date().toISOString(),
          });
        }

        await this.recordToolTrace("plasm", "plasm.live_run", started, {
          intent: session.intent,
          logical_session_ref: session.logicalSessionRef,
          plan_commit_ref: input.runRef,
          run_id: runId,
          catalog_cgs_hash: catalogCgsHash,
          ok: result.ok,
          trace_id: activeTraceId() ?? span.spanContext().traceId,
        });

        await this.emitHook("run:complete", {
          intent: session.intent,
          planCommitRef: input.runRef,
          runRef: input.runRef,
          runId,
          ok: result.ok,
          logicalSessionRef: session.logicalSessionRef,
        });

        return formatPlasmRunMarkdown(result.message, result.ok, result.rowsJson);
      },
    );
  }

  private async emitHook(
    event: "plan:commit" | "run:complete",
    detail: Record<string, unknown>,
  ): Promise<void> {
    if (!this.hookRunner || !this.getAuthoringContext) return;
    await this.hookRunner.emit(event, this.getAuthoringContext(), detail);
  }

  private catalogHashForEntry(entryId: string): string | undefined {
    return this.loadedCatalogs.find((c) => c.manifest.entryId === entryId)?.manifest.cgsHash;
  }

  private async recordToolTrace(
    kind: string,
    name: string,
    startedMs: number,
    attributes: Record<string, string | number | boolean | undefined>,
  ): Promise<void> {
    if (!this.archive) return;
    const tenantId = this.sessionManager.tenant();
    const traceId = (attributes.trace_id as string | undefined) ?? randomUUID();
    const cleaned: Record<string, string | number | boolean> = {};
    for (const [key, value] of Object.entries(attributes)) {
      if (value !== undefined) cleaned[key] = value;
    }
    await this.archive.recordToolEvent(tenantId, traceId, kind, name, cleaned);
    await this.archive.finalizeTrace({
      summary: {
        trace_id: traceId,
        tenant_id: tenantId,
        logical_session_ref:
          typeof cleaned.logical_session_ref === "string"
            ? cleaned.logical_session_ref
            : undefined,
        intent: typeof cleaned.intent === "string" ? cleaned.intent : undefined,
        status: "completed",
        started_at_ms: startedMs,
        ended_at_ms: Date.now(),
        project_slug: "main",
        totals: { tool_calls: 1 },
      },
      records: [
        {
          at_ms: startedMs,
          kind,
          name,
          attributes: cleaned,
        },
      ],
    });
  }

  private async requireSessionByRef(ref: string): Promise<AgentSessionState> {
    const session = await this.sessionManager.getByLogicalRef(ref);
    if (!session) {
      throw new Error(
        `unknown logical_session_ref \`${ref}\` — call plasm_context first with a stable intent`,
      );
    }
    return session;
  }
}
