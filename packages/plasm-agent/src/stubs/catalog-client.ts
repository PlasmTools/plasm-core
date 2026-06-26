import type { HostTransportFn } from "../engine/napi-binding.js";
import { createStubHostTransport } from "../engine/create-host-transport.js";
import {
  createEngine,
  type DryRunResult,
  type PlasmEngine,
} from "../engine/napi-binding.js";
import { createDefaultHostTransport } from "../engine/host-transport.js";
import { createFixtureMockTransport } from "../engine/fixture-mock-transport.js";
import type { ProgramBuilder, ProgramBuilderOptions } from "./program-builder.js";
import { createProgramBuilder } from "./program-builder.js";

export interface CatalogClientOptions extends ProgramBuilderOptions {
  engine?: PlasmEngine;
  intent?: string;
  transport?: HostTransportFn;
}

export interface StubInvokeOptions {
  transport?: HostTransportFn;
  engine?: PlasmEngine;
}

const sessionInit = new WeakMap<ProgramBuilder, Promise<void>>();

/** Load catalog + register deterministic `e#` symbols for program execution (not agent teaching). */
export async function ensureStubSession(builder: ProgramBuilder): Promise<void> {
  const existing = sessionInit.get(builder);
  if (existing) return existing;

  const task = (async () => {
    const catalogRoot = builder.catalogRoot;
    const engine = builder.engine ?? createEngine();
    if (catalogRoot) {
      await engine.loadCatalog({
        rootDir: catalogRoot,
        manifest: { entryId: builder.entryId },
      });
    }
    const entities = [...(builder.stubEntities ?? [])].sort((a, b) => a.localeCompare(b));
    if (!entities.length) return;
    await engine.synthesizeTeaching(
      "",
      entities.map((entity) => ({ api: builder.entryId, entity })),
    );
  })();
  sessionInit.set(builder, task);
  return task;
}

/** @deprecated Use {@link ensureStubSession}. */
export async function ensureCatalogLoaded(builder: ProgramBuilder): Promise<void> {
  return ensureStubSession(builder);
}

export function createCatalogClient(options: CatalogClientOptions): ProgramBuilder {
  return createProgramBuilder({
    entryId: options.entryId,
    cgsHash: options.cgsHash,
    engine: options.engine ?? createEngine(),
    logicalSessionRef: options.logicalSessionRef,
    agentSessionId: options.agentSessionId,
    catalogRoot: options.catalogRoot,
    stubEntities: options.stubEntities,
  });
}

export interface ExecuteRowsResult<TRow> {
  planCommitRef: string;
  summary: string;
  rows: TRow[];
}

/** Plasm string literal from a TypeScript string (safe quoting). */
export function plasmLiteral(value: string): string {
  return JSON.stringify(value);
}

/** Plasm numeric literal from a TypeScript number. */
export function plasmNumber(value: number | undefined): string {
  if (value === undefined || Number.isNaN(value)) return "0";
  return String(value);
}

/** Plasm boolean literal. */
export function plasmBoolean(value: boolean | undefined): string {
  return value ? "true" : "false";
}

export interface DottedArgSpec {
  key: string;
  value: unknown;
  kind?: "literal" | "number" | "boolean" | "select";
  optional?: boolean;
}

/** Build `key=value` dotted arg list for Plasm invoke (omits undefined optional keys). */
export function buildDottedArgs(specs: DottedArgSpec[]): string {
  const parts: string[] = [];
  for (const { key, value, kind = "literal", optional } of specs) {
    if (optional && value === undefined) continue;
    const emit =
      kind === "number"
        ? plasmNumber(value as number)
        : kind === "boolean"
          ? plasmBoolean(value as boolean)
          : plasmLiteral(String(value ?? ""));
    parts.push(`${key}=${emit}`);
  }
  return parts.join(", ");
}

function parseRowsJson<TRow>(rowsJson: string | undefined): TRow[] {
  if (!rowsJson?.trim()) return [];
  const parsed = JSON.parse(rowsJson) as unknown;
  if (Array.isArray(parsed)) return parsed as TRow[];
  if (parsed && typeof parsed === "object" && Array.isArray((parsed as { results?: unknown }).results)) {
    return (parsed as { results: TRow[] }).results;
  }
  return [];
}

function extractRowsFromLive(live: {
  rowsJson?: string;
  message?: string;
}): unknown[] {
  if (live.rowsJson) {
    return parseRowsJson(live.rowsJson);
  }
  return [];
}

/** Dry-run then live-execute a program; parse entity rows from NAPI output. */
export async function executeRows<TRow>(
  builder: ProgramBuilder,
  program: string,
  options?: StubInvokeOptions,
): Promise<ExecuteRowsResult<TRow>> {
  await ensureStubSession(builder);
  const bound = builder.program(program);
  const dry: DryRunResult = await bound.dryRun();
  const transport =
    options?.transport ??
    (process.env.PLASM_STUB_USE_MOCK_TRANSPORT === "1"
      ? createFixtureMockTransport()
      : createStubHostTransport(builder.entryId));

  if (typeof bound.run !== "function") {
    throw new Error("executeRows: program builder missing run()");
  }
  const live = (await bound.run(dry.planCommitRef, transport)) as {
    rowsJson?: string;
    message?: string;
  };
  const rows = extractRowsFromLive(live) as TRow[];
  return {
    planCommitRef: dry.planCommitRef,
    summary: dry.summary,
    rows,
  };
}

/** Dry-run only — plan review without live HTTP. */
export async function dryRunProgram(
  builder: ProgramBuilder,
  program: string,
): Promise<DryRunResult> {
  await ensureStubSession(builder);
  return builder.program(program).dryRun();
}
