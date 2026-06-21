export interface OperatorStubFreshness {
  stubPath: string;
  liveCatalogCgsHash: string;
  generatedCatalogCgsHash: string | null;
  fresh: boolean;
  lastBuiltAt: string | null;
  validationErrors: string[];
}

export interface OperatorCatalogEntry {
  entryId: string;
  label: string;
  rootDir: string;
  catalogCgsHash: string;
  authScheme?: string;
  entityCount: number;
  capabilityCount: number;
  stub: OperatorStubFreshness;
}

export interface OperatorCatalogsResponse {
  catalogs: OperatorCatalogEntry[];
  generatedAt: string;
}

export interface OperatorSessionEntry {
  intent: string;
  logicalSessionRef: string;
  logicalSessionId: string;
  waveCount: number;
  seedCount: number;
  planCommitCount: number;
  updatedAt: string;
}

export interface OperatorSessionsResponse {
  sessions: OperatorSessionEntry[];
}

export interface OperatorPlanCommit {
  intent: string;
  logicalSessionRef: string;
  ref: string;
  program: string;
  at: string;
}

export interface OperatorPlansResponse {
  plans: OperatorPlanCommit[];
}

export interface OperatorOpsResponse {
  nativeEngineAvailable: boolean;
  engineMode: "napi" | "stub";
  agentRoot: string;
  catalogCount: number;
  sessionCount: number;
  planCommitCount: number;
}

export interface OperatorHealthResponse {
  status: string;
}
