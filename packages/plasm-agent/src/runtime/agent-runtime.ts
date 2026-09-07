import path from "node:path";
import { createHash, randomUUID } from "node:crypto";
import { mkdir, writeFile } from "node:fs/promises";

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
  /**
   * Directory for materializing run snapshots for harness TS transforms.
   * Defaults to `PLASM_RUN_ARTIFACTS_DIR` or `<agentRoot>/.plasm/artefacts`.
   */
  artefactWorkspaceRoot?: string;
}

export interface DiscoverInput {
  intent: string;
}

export interface PlasmContextInput {
  intent: string;
  sessionMode?: "new" | "extend";
  seeds?: Array<{ api: string; entity: string }>;
  logicalSessionRef?: string;
  rankedCapabilities?: string[] | null;
  /** When true, `new` without seeds auto-picks via discover; seeds on `new` are rejected. */
  autoSeed?: boolean;
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

export interface PlasmReadRunArtifactInput {
  logicalSessionRef: string;
  runId: string;
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
  readonly artefactWorkspaceRoot: string;
  private readonly agentRoot: string;
  private readonly archiveEnabled: boolean;
  private readonly hookRunner?: HookRunner;
  private readonly getAuthoringContext?: () => AuthoringContext;
  private loadedCatalogs: LoadedCatalog[] = [];
  /**
   * One auto-seed workflow session per AgentRuntime process.
   * Models often rephrase `intent` on repeat `new` — do not remint or re-FO.
   * Holds the live AgentSessionState (not a detached card) so plasm / plasm_run
   * resolve without depending on durable store list scans.
   */
  private workflowSession: AgentSessionState | null = null;

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
    const envArtefacts = process.env.PLASM_RUN_ARTIFACTS_DIR?.trim();
    this.artefactWorkspaceRoot = path.resolve(
      config.artefactWorkspaceRoot ??
        envArtefacts ??
        path.join(this.agentRoot, ".plasm", "artefacts"),
    );
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
    await mkdir(this.artefactWorkspaceRoot, { recursive: true });
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
    const sessionMode = input.sessionMode ?? "new";
    const autoSeed = input.autoSeed ?? false;
    let seeds = input.seeds ?? [];

    return plasmSpans.toolContext(
      {
        intent,
        entryId: seeds[0]?.api,
        catalogCgsHash: seeds[0]?.api ? this.catalogHashForEntry(seeds[0].api) : undefined,
      },
      async (span) => {
        if (!intent) throw new Error("plasm_context requires `intent`");

        if (sessionMode === "extend") {
          const ref = input.logicalSessionRef?.trim();
          const session = ref
            ? await this.requireSessionByRef(ref)
            : (this.workflowSession ?? (await this.sessionManager.get(intent)));
          if (!session) {
            throw new Error(
              "plasm_context extend: unknown logical_session_ref — call session_mode new first",
            );
          }
          this.workflowSession = session;
          if (seeds.length) {
            const before = new Set(session.seeds.map(seedKey));
            const merged = mergeSeeds(session.seeds, seeds);
            const hasNew = merged.length > before.size;
            const started = Date.now();
            const exposure = await this.engine.synthesizeTeaching(intent, seeds);
            if (exposure.tsv.trim()) {
              session.teachingTsv = session.teachingTsv
                ? `${session.teachingTsv.trim()}\n\n${exposure.tsv.trim()}`
                : exposure.tsv.trim();
              session.waves.push({
                entryId: seeds[0]?.api ?? "unknown",
                entities: seeds.map((s) => s.entity),
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
              session_mode: "extend",
              trace_id: activeTraceId() ?? span.spanContext().traceId,
            });
            return formatPlasmContextMarkdown(
              session.logicalSessionRef,
              exposure.tsv,
              !hasNew && !exposure.tsv.trim(),
            );
          }
          span.setAttribute("plasm.logical_session_ref", session.logicalSessionRef);
          // Re-surface the full teaching card — empty deltas still need e#/m# visible.
          return formatPlasmContextMarkdown(
            session.logicalSessionRef,
            session.teachingTsv,
            true,
          );
        }

        // Idempotent auto-seed `new`: return the open workflow session before FO / expose.
        if (autoSeed && this.workflowSession) {
          span.setAttribute("plasm.logical_session_ref", this.workflowSession.logicalSessionRef);
          return formatPlasmContextMarkdown(
            this.workflowSession.logicalSessionRef,
            this.workflowSession.teachingTsv,
            true,
          );
        }

        if (autoSeed) {
          if (seeds.length) {
            throw new Error(
              'plasm_context auto-seed: omit `seeds` on session_mode "new" (host selects entities from intent)',
            );
          }
          if (typeof this.engine.selectAutoSeeds !== "function") {
            throw new Error(
              "plasm_context auto-seed: engine missing selectAutoSeeds — rebuild @plasm_lang/engine (semantic spine)",
            );
          }
          const routed = await this.engine.selectAutoSeeds(intent);
          if (routed.decision !== "ready" || !routed.seeds.length) {
            return [
              `**plasm_context** auto-seed: ${routed.decision} (no session minted).`,
              routed.reasoning ? `Reasoning: ${routed.reasoning}` : "",
              "Rephrase intent with the provider brand, or pass routing_ref + clarify_choice when offered.",
              "",
              routed.markdown.trim() || "(no breakout markdown)",
            ]
              .filter(Boolean)
              .join("\n");
          }
          seeds = routed.seeds;
        } else if (!seeds.length) {
          throw new Error(
            "plasm_context requires non-empty `seeds` when auto-seed is off (or set PLASM_DISCOVERY_SEMANTIC_AUTO_SEED=1)",
          );
        }

        void input.rankedCapabilities;

        const entryId = seeds[0]?.api;
        const catalogCgsHash = entryId ? this.catalogHashForEntry(entryId) : undefined;
        if (catalogCgsHash) span.setAttribute("plasm.catalog_cgs_hash", catalogCgsHash);

        const ids = mintLogicalSessionId(
          this.sessionManager.tenant(),
          `${intent}\0new\0${Date.now()}`,
        );
        const session = await this.sessionManager.getOrCreate(
          `${intent}\0${ids.logicalSessionRef}`,
          ids.logicalSessionRef,
          ids.logicalSessionId,
        );

        const started = Date.now();
        const exposure = await this.engine.synthesizeTeaching(intent, seeds);
        if (!exposure.tsv.trim()) {
          throw new Error(
            "plasm_context new: empty language card after seed exposure — host defect (check catalog entry_id binding)",
          );
        }
        session.seeds = seeds;
        session.teachingTsv = exposure.tsv.trim();
        session.waves = [
          {
            entryId: seeds[0]?.api ?? "unknown",
            entities: seeds.map((s) => s.entity),
            tsv: exposure.tsv,
            at: new Date().toISOString(),
          },
        ];
        session.planCommits = [];
        await this.sessionManager.update(session);
        this.workflowSession = session;

        span.setAttribute("plasm.logical_session_ref", session.logicalSessionRef);
        await this.recordToolTrace("tool", "plasm_context", started, {
          intent,
          logical_session_ref: session.logicalSessionRef,
          entry_id: entryId,
          session_mode: "new",
          auto_seed: autoSeed,
          trace_id: activeTraceId() ?? span.spanContext().traceId,
        });

        return formatPlasmContextMarkdown(session.logicalSessionRef, exposure.tsv, false);
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

        await this.materializeRunArtefact(runId, {
          run_id: runId,
          plan_commit_ref: input.runRef,
          message: result.message,
          results: parseRowsJson(result.rowsJson),
          ok: result.ok,
        });

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

        return formatPlasmRunMarkdown(result.message, result.ok, result.rowsJson, runId);
      },
    );
  }

  async readRunArtifact(input: PlasmReadRunArtifactInput): Promise<string> {
    void input.reasoning;
    await this.requireSessionByRef(input.logicalSessionRef);
    if (!this.archive || typeof this.archive.getRun !== "function") {
      throw new Error("plasm_read_run_artifact: archive store unavailable");
    }
    const snap = await this.archive.getRun(input.runId.trim());
    if (!snap) {
      throw new Error(`plasm_read_run_artifact: unknown run_id \`${input.runId}\``);
    }
    if (
      snap.logical_session_ref &&
      snap.logical_session_ref !== input.logicalSessionRef.trim()
    ) {
      throw new Error(
        "plasm_read_run_artifact: run_id does not belong to this logical_session_ref",
      );
    }
    await this.materializeRunArtefact(snap.run_id, snap);
    return [
      `**run_id:** \`${snap.run_id}\``,
      "Materialized under artefact workspace for **plasm_artefact_transform**.",
      "",
      "```json",
      JSON.stringify(snap, null, 2),
      "```",
    ].join("\n");
  }

  private async materializeRunArtefact(runId: string, payload: unknown): Promise<void> {
    const safe = runId.replace(/[^a-zA-Z0-9_-]/g, "");
    if (!safe) return;
    const dir = path.join(this.artefactWorkspaceRoot, "artefacts");
    await mkdir(dir, { recursive: true });
    const body = `${JSON.stringify(payload, null, 2)}\n`;
    await writeFile(path.join(dir, `${safe}.json`), body, "utf8");
    await writeFile(path.join(dir, "latest.json"), body, "utf8");
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
    const key = ref.trim();
    if (this.workflowSession?.logicalSessionRef === key) {
      return this.workflowSession;
    }
    const session = await this.sessionManager.getByLogicalRef(key);
    if (!session) {
      const open = this.workflowSession?.logicalSessionRef;
      throw new Error(
        open
          ? `unknown logical_session_ref \`${key}\` — open workflow is \`${open}\` (reuse it verbatim)`
          : `unknown logical_session_ref \`${key}\` — call plasm_context first with a stable intent`,
      );
    }
    this.workflowSession = session;
    return session;
  }
}
