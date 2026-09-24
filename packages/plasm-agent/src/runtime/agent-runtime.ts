import { routingExplanationLines, routingRecoveryMarkdown } from "../engine/routing.js";
import path from "node:path";
import { createHash, randomUUID } from "node:crypto";
import { mkdir, readFile, writeFile } from "node:fs/promises";


import type { LoadedCatalog } from "../catalog/loader.js";
import { FilesystemCatalogLoader } from "../catalog/loader.js";
import {
  createEngine,
  type HostTransportFn,
  type PlasmEngine,
} from "../engine/napi-binding.js";
import { createDefaultHostTransport } from "../engine/host-transport.js";
import { formatLogicalSessionWireRef } from "../runtime/logical-session.js";
import { logicalSessionRefSchema, workflowIntentSchema, deriveIntent, type IntentProvenance } from "./session-contract.js";
import { SessionManager, type AgentSessionState } from "../session-state.js";
import { runIdFromArtifactRef } from "../tools/artifact-contract.js";
import {
  formatPlasmContextMarkdown,
  formatPlasmDryRunMarkdown,
  formatPlasmRunMarkdown,
  writeCountFromSummary,
} from "../tools/format.js";
import { LocalArchiveStore } from "../archive/index.js";
import { createArchiveStore } from "../archive/resolve-backend.js";
import type { ProdArchiveStore } from "../archive/prod-archive-store.js";
import { z } from "zod";
import { artifactRuntimeAvailable } from "../tools/artifact-process.js";
import { writeWorkspaceFile } from "../tools/workspace-files.js";
import { activeTraceId, plasmSpans } from "../telemetry/plasm-spans.js";
import { PlasmSpanAttributes } from "../instrumentation.js";
import type { AuthoringContext } from "../authoring/context.js";
import type { HookRunner } from "../authoring/hook-runner.js";
import type { AgentWorkflowWorldDefinition } from "../define-agent.js";
import { createAgentStateStore } from "../state/define-state.js";
import { DiscoveryQueue } from "./discovery-queue.js";

export type AgentArchiveStore = LocalArchiveStore | ProdArchiveStore;

export interface AgentRuntimeConfig {
  agentRoot: string;
  /** Optional first intent supplied by the embedding workflow. */
  initialIntent?: string;
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
  logicalSessionRef?: string;
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
  runId?: string;
  artifactUri?: string;
  reasoning?: string;
}

function resolveReadRunId(input: PlasmReadRunArtifactInput): string {
  const runId = input.runId?.trim() ?? "";
  const uri = input.artifactUri?.trim() ?? "";
  if (Boolean(runId) === Boolean(uri)) {
    throw new Error("plasm_read_run_artifact: provide exactly one of run_id or artifact_uri");
  }
  const resolved = runIdFromArtifactRef(runId || uri);
  if (!resolved) {
    throw new Error(
      uri
        ? "plasm_read_run_artifact: artifact_uri must contain a pr… run_id"
        : `plasm_read_run_artifact: invalid run_id \`${runId}\``,
    );
  }
  return resolved;
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
  private readonly engineInstanceId = randomUUID();
  readonly engine: PlasmEngine;
  readonly sessionManager: SessionManager;
  readonly archive: AgentArchiveStore | null;
  readonly hostTransport: HostTransportFn | null;
  readonly artefactWorkspaceRoot: string;
  private readonly agentRoot: string;
  private readonly initialProvenance?: IntentProvenance;
  private readonly archiveEnabled: boolean;
  private readonly hookRunner?: HookRunner;
  private readonly getAuthoringContext?: () => AuthoringContext;
  private loadedCatalogs: LoadedCatalog[] = [];
  /** Most recently used live session; explicit new calls create distinct workflows. */
  private workflowSession: AgentSessionState | null = null;

  /** True after the first successful `plasm_read_run_artifact` this runtime. */
  private artefactMaterialized = false;

  /** True after the first successful `plasm_context` mint on this runtime. */
  hasOpenWorkflow(): boolean {
    return this.workflowSession != null;
  }

  /** Successful `plasm_run` write-node count this process (fused 0w reads stay 0). */
  private committedWriteOps = 0;

  /** True after a run snapshot has been written under the artefact workspace. */
  hasMaterializedArtefact(): boolean {
    return this.artefactMaterialized;
  }

  /** Live write ops committed via `plasm_run` on this runtime. */
  committedLiveWriteOps(): number {
    return this.committedWriteOps;
  }

  /** Open-wave teaching TSV (empty before mint). */
  openWorkflowTeachingTsv(): string | undefined {
    const tsv = this.workflowSession?.teachingTsv?.trim();
    return tsv || undefined;
  }

  constructor(config: AgentRuntimeConfig) {
    this.agentRoot = path.resolve(config.agentRoot);
    this.initialProvenance = config.initialIntent === undefined ? undefined : deriveIntent(undefined, workflowIntentSchema.parse(config.initialIntent));
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
    const bindingPath = process.env.PLASM_DISCOVERY_BINDINGS_PATH?.trim()
      ?? path.join(this.agentRoot, "catalogs", "deployment-bindings.json");
    const deploymentId = process.env.PLASM_DISCOVERY_DEPLOYMENT?.trim()
      || createHash("sha256").update(`${this.agentRoot}\0${this.sessionManager.tenant()}`).digest("hex");
    await this.engine.activateDiscovery(deploymentId, await readFile(bindingPath, "utf8"));
    this.loadedCatalogs = catalogs;
    return catalogs;
  }

  listCatalogs(): LoadedCatalog[] {
    return [...this.loadedCatalogs];
  }

  private readonly discoveryQueue = new DiscoveryQueue();

  async plasmContext(input: PlasmContextInput): Promise<string> {
    // Snapshot inputs before waiting: callers must not mutate a queued request.
    const request: PlasmContextInput = {
      ...input,
      intent: workflowIntentSchema.parse(input.intent),
    };
    if (request.sessionMode !== "extend") return this.plasmContextTurn(request);
    const ref = logicalSessionRefSchema.parse(request.logicalSessionRef);
    return this.discoveryQueue.run(
      ref,
      request.intent,
      () => this.plasmContextTurn(request),
    );
  }

  private async plasmContextTurn(input: PlasmContextInput): Promise<string> {
    const intent = workflowIntentSchema.parse(input.intent);
    const mode = input.sessionMode ?? "new";
    if (!intent) throw new Error("plasm_context requires intent");
    return plasmSpans.toolContext({ intent }, async (span) => {
      const started = Date.now();
      const existing = mode === "extend"
        ? await this.requireSessionByRef(input.logicalSessionRef ?? "")
        : undefined;
      if (mode === "new" && input.logicalSessionRef) {
        throw new Error("logical_session_ref belongs on session_mode extend");
      }
      const provenance = deriveIntent(existing?.intentProvenance ?? this.initialProvenance, intent);
      const packet = await this.engine.routeIntent(
        provenance,
        existing?.logicalSessionId,
      );
      const { routing, teaching } = packet;
      if (JSON.stringify(routing.intent_provenance) !== JSON.stringify(provenance)) throw new Error("Routing changed intent provenance");
      if (existing && (existing.logicalSessionId !== routing.pin_id
        || existing.registryGeneration !== routing.retrieval.generation)) {
        throw new Error("Routing changed a logical session's pinned registry generation");
      }
      const wireRef = logicalSessionRefSchema.parse(formatLogicalSessionWireRef(Buffer.from(routing.pin_id.replace(/-/g, ""), "hex")));
      const session = existing ?? await this.sessionManager.getOrCreate({
        intent: provenance.nodes[0]!.intent, intentProvenance: provenance, logicalSessionRef: wireRef, logicalSessionId: routing.pin_id,
      });
      session.intentProvenance = provenance;
      session.engineInstanceId = this.engineInstanceId;
      session.registryGeneration = routing.retrieval.generation;
      await this.sessionManager.update(session);
      this.workflowSession = session;
      const recoveryMarkdown = routingRecoveryMarkdown(routing);
      if (!routing.closure) {
        await this.recordToolTrace("tool", "plasm_context", started, {
          intent, session_mode: mode, routing: JSON.stringify(routing),
          logical_session_ref: session.logicalSessionRef,
        });
        return [
          routing.intent_analysis,
          recoveryMarkdown ?? "**plasm_context:** no relevant capabilities selected",
          `**logical_session_ref:** \`${session.logicalSessionRef}\``,
          ...(recoveryMarkdown ? [] : routingExplanationLines(routing.matching)),
        ].filter(Boolean).join("\n\n");
      }
      if (!routing.closure || !teaching?.tsv.trim()) {
        throw new Error("Routing is missing its prerequisite closure or canonical teaching");
      }
      // Keep exact capability and prerequisite distinctions as returned by Rust.
      session.routingClosures = [...(session.routingClosures ?? []), routing.closure];
      const exposed = teaching.delta_refs.map((ref) => {
        const separator = ref.indexOf(":");
        if (separator < 1) throw new Error("Invalid native teaching entity reference");
        return { api: ref.slice(0, separator), entity: ref.slice(separator + 1) };
      });
      session.seeds = mergeSeeds(session.seeds, exposed);
      session.teachingTsv = [session.teachingTsv.trim(), teaching.tsv.trim()].filter(Boolean).join("\n\n");
      session.waves.push({
        entryId: exposed[0]?.api ?? "unknown", entities: exposed.map((entry) => entry.entity),
        tsv: teaching.tsv, at: new Date().toISOString(),
      });
      await this.sessionManager.update(session);
      this.workflowSession = session;
      span.setAttribute("plasm.logical_session_ref", session.logicalSessionRef);
      await this.recordToolTrace("tool", "plasm_context", started, {
        intent, logical_session_ref: session.logicalSessionRef, session_mode: mode,
        registry_generation: session.registryGeneration, routing: JSON.stringify(routing),
        trace_id: activeTraceId() ?? span.spanContext().traceId,
      });
      const teachingMarkdown = formatPlasmContextMarkdown(session.logicalSessionRef, teaching.tsv, false);
      return [routing.intent_analysis, teachingMarkdown, recoveryMarkdown].filter(Boolean).join("\n\n");
    });
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
        const dry = await this.engine.dryRun(input.program, session.logicalSessionId);
        session.planCommits.push({
          ref: dry.planCommitRef,
          program: input.program,
          at: new Date().toISOString(),
          writeCount: writeCountFromSummary(dry.summary),
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

        if (dry.fusedCleanRead) {
          return this.plasmRun({
            logicalSessionRef: input.logicalSessionRef,
            runRef: dry.planCommitRef,
          });
        }

        return formatPlasmDryRunMarkdown(dry.summary, dry.planCommitRef);
      },
    );
  }

  async plasmRun(input: PlasmRunInput): Promise<string> {
    const session = await this.requireSessionByRef(input.logicalSessionRef);
    void input.reasoning;
    const entryId = session.seeds[0]?.api;
    const catalogCgsHash = entryId ? this.catalogHashForEntry(entryId) : undefined;

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
        if (this.hostTransport && !this.engine.runPlanLive) {
          throw new Error("Engine does not support reviewed live execution");
        }
        const result = this.hostTransport
          ? await this.engine.runPlanLive!(input.runRef, this.hostTransport, session.logicalSessionId)
          : await this.engine.runPlan(input.runRef, session.logicalSessionId);
        const runMeta = parseRunMeta(result.metaJson);
        const requestFingerprints = collectRequestFingerprints(runMeta);
        const artifacts = result.ok
          ? z.array(z.object({ run_id: z.string().regex(/^pr[a-f0-9]{64}$/), snapshot: z.unknown() }))
            .parse(JSON.parse(result.artifactsJson ?? "null"))
          : [];
        for (const artifact of artifacts) {
          if (this.archive) {
            await this.archive.writeRunSnapshot({
              run_id: artifact.run_id, plan_commit_ref: input.runRef,
              logical_session_ref: session.logicalSessionRef, intent: session.intent,
              ok: result.ok, message: result.message,
              native_snapshot: artifact.snapshot,
              _meta: { plasm: { steps: runMeta?.steps ?? [], request_fingerprints: requestFingerprints } },
              archived_at: new Date().toISOString(),
            });
          }
          await this.materializeRunArtefact(artifact.run_id, artifact.snapshot, session.logicalSessionRef);
        }
        const runId = artifacts[0]?.run_id;
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

        if (result.ok) {
          const commit = session.planCommits.find((c) => c.ref === input.runRef);
          this.committedWriteOps += commit?.writeCount ?? 0;
        }

        const markdown = formatPlasmRunMarkdown(
          result.message,
          result.ok,
          runId,
        );
        return markdown + artifacts.slice(1).map(artifact =>
          `\n\n**run_id:** \`${artifact.run_id}\` — read with plasm_read_run_artifact.`).join("");
      },
    );
  }

  async readRunArtifact(input: PlasmReadRunArtifactInput): Promise<string> {
    void input.reasoning;
    await this.requireSessionByRef(input.logicalSessionRef);
    if (!this.archive || typeof this.archive.getRun !== "function") {
      throw new Error("plasm_read_run_artifact: archive store unavailable");
    }
    const runId = resolveReadRunId(input);
    const snap = await this.archive.getRun(runId, input.logicalSessionRef.trim());
    if (!snap) {
      throw new Error(`plasm_read_run_artifact: unknown run_id \`${runId}\``);
    }
    if (
      snap.logical_session_ref &&
      snap.logical_session_ref !== input.logicalSessionRef.trim()
    ) {
      throw new Error(
        "plasm_read_run_artifact: run_id does not belong to this logical_session_ref",
      );
    }
    await this.materializeRunArtefact(snap.run_id, snap.native_snapshot ?? snap, input.logicalSessionRef.trim());
    this.artefactMaterialized = true;
    return [
      `**run_id:** \`${snap.run_id}\``,
      artifactRuntimeAvailable()
        ? "Pass this file locator in **plasm_artefact_transform.paths** and write a default TypeScript function over the parsed artifacts; return only the needed derived result."
        : "Materialized under artefact workspace.",
      `File: artefacts/${input.logicalSessionRef.trim()}/${snap.run_id}.json`,
      "Snapshot contents are available only to programmatic processing; they are not inserted into model context.",
    ].join("\n");
  }

  private async materializeRunArtefact(runId: string, payload: unknown, logicalSessionRef: string): Promise<void> {
    const safe = runId.replace(/[^a-zA-Z0-9_-]/g, "");
    if (!safe) return;
    await mkdir(this.artefactWorkspaceRoot, { recursive: true });
    const body = `${JSON.stringify(payload, null, 2)}\n`;
    await writeWorkspaceFile(this.artefactWorkspaceRoot, `artefacts/${logicalSessionRef}/${safe}.json`, body);
    await writeWorkspaceFile(this.artefactWorkspaceRoot, "artefacts/latest.json", body);
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
    if (session.engineInstanceId !== this.engineInstanceId) {
      throw new Error("Execution session expired with its native engine; explicitly open a new context");
    }
    this.workflowSession = session;
    return session;
  }
}
