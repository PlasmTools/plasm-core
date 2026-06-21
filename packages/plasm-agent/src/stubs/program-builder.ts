import {
  createEngine,
  type DryRunResult,
  type HostTransportFn,
  type PlasmEngine,
} from "../engine/napi-binding.js";
import { createDefaultHostTransport } from "../engine/host-transport.js";

export interface ProgramBuilderOptions {
  entryId: string;
  cgsHash: string;
  engine?: PlasmEngine;
  logicalSessionRef?: string;
  agentSessionId?: string;
  catalogRoot?: string;
  /** Entity names to expose before dry-run / live execute. */
  stubEntities?: string[];
}

export interface ProgramBuilderProvenance {
  entryId: string;
  catalogCgsHash: string;
  logicalSessionRef?: string;
  agentSessionId?: string;
}

export interface ProgramBuilder extends ProgramBuilderProvenance {
  readonly catalogRoot?: string;
  readonly stubEntities?: string[];
  readonly engine?: PlasmEngine;
  readonly programSource: string;
  program(source: string): ProgramBuilder;
  dryRun(): Promise<DryRunResult>;
  run(planCommitRef: string, transport?: HostTransportFn): Promise<unknown>;
}

function cloneBuilder(
  opts: ProgramBuilderOptions,
  programSource: string,
): ProgramBuilder {
  const engine = opts.engine ?? createEngine();
  const provenance: ProgramBuilderProvenance = {
    entryId: opts.entryId,
    catalogCgsHash: opts.cgsHash,
    logicalSessionRef: opts.logicalSessionRef,
    agentSessionId: opts.agentSessionId,
  };

  return {
    ...provenance,
    catalogRoot: opts.catalogRoot,
    stubEntities: opts.stubEntities,
    engine,
    programSource,
    program(source: string) {
      return cloneBuilder(opts, source);
    },
    async dryRun() {
      return engine.dryRun(programSource);
    },
    async run(planCommitRef: string, transport?: HostTransportFn) {
      const hostTransport = transport ?? createDefaultHostTransport();
      if (typeof engine.runPlanLive === "function") {
        const live = await engine.runPlanLive(planCommitRef, hostTransport);
        if (!live.ok) {
          throw new Error(live.message);
        }
        return live;
      }
      const validation = await engine.runPlan(planCommitRef);
      if (!validation.ok) {
        throw new Error(validation.message);
      }
      return validation;
    },
  };
}

/** Typed program builder — dryRun/run delegate to NAPI PlasmEngine. */
export function createProgramBuilder(opts: ProgramBuilderOptions): ProgramBuilder {
  return cloneBuilder(opts, "");
}
