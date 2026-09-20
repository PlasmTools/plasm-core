export {
  createAgentFromDefinition,
  createAgentFromProject,
  createPlasmAgentConfig,
  defineAgent,
  resolveAgentDefinition,
} from "./define-agent.js";
export type {
  AgentBuildConfig,
  AgentCompactionConfig,
  AgentDefinition,
  AgentExperimentalConfig,
  AgentModelOptions,
  AgentWorkflowDefinition,
  AgentWorkflowWorldDefinition,
  CreateAgentFromDefinitionOptions,
  ResolvedAgentDefinition,
} from "./define-agent.js";

export {
  createAuthoringContext,
} from "./authoring/context.js";
export type { AuthoringContext, CreateAuthoringContextOptions } from "./authoring/context.js";
export { defineChannel, validateChannelRoute, isChannelDefinition } from "./authoring/define-channel.js";
export type {
  ChannelDefinition,
  ChannelHandler,
  ChannelRoute,
  DefineChannelInput,
  HttpMethod,
} from "./authoring/define-channel.js";
export { defineHook, isHookDefinition } from "./authoring/define-hook.js";
export type { DefineHookInput, HookDefinition, HookEvent, HookHandler } from "./authoring/define-hook.js";
export { HookRunner } from "./authoring/hook-runner.js";
export { defineSchedule, cronIntervalMs, isScheduleDefinition } from "./authoring/define-schedule.js";
export type { DefineScheduleInput, ScheduleDefinition, ScheduleHandler } from "./authoring/define-schedule.js";
export { defineSkill, isSkillDefinition } from "./authoring/define-skill.js";
export type { DefineSkillInput, SkillDefinition } from "./authoring/define-skill.js";
export {
  loadAuthoredSlots,
  summarizeLoadedSlots,
} from "./authoring/slot-loader.js";
export type {
  LoadedChannel,
  LoadedHook,
  LoadedProjectSlots,
  LoadedSchedule,
  LoadedSkill,
  LoadedSlotsSummary,
  LoadAuthoredSlotsOptions,
} from "./authoring/slot-loader.js";
export { tryHandleChannelRoute, listChannelRoutes } from "./authoring/channel-dispatch.js";
export {
  exportScheduleCronManifest,
  exportScheduleTaskManifest,
  startScheduleTimers,
  tryHandleScheduleDevDispatch,
} from "./authoring/schedule-manager.js";
export type { ScheduleCronManifest, ScheduleHandle, ScheduleTaskManifest } from "./authoring/schedule-manager.js";
export {
  createSubagentRegistry,
  loadSubagents,
  summarizeSubagents,
} from "./authoring/subagent-loader.js";
export type { LoadedSubagent, SubagentRegistry } from "./authoring/subagent-loader.js";
export {
  createArtefactTransformTool,
  createCompleteTaskTool,
  createEvalTerminalTools,
  createHarnessTools,
  createSubmitAnswerTool,
  renderSkillIndex,
  runArtefactTransform,
  COMPLETE_TASK_TOOL_DESCRIPTION,
  EVAL_TERMINAL_REQUIRES_DISCOVERY,
  PLASM_ARTEFACT_TRANSFORM_TOOL_DESCRIPTION,
  SUBMIT_ANSWER_TOOL_DESCRIPTION,
} from "./tools/harness-tools.js";
export type { EvalTerminalGate } from "./tools/harness-tools.js";
export {
  TASK_LEDGER_MAX_BYTES,
  TASK_LEDGER_STATE_HEADER,
  TASK_LEDGER_TOOL_DESCRIPTION,
  TASK_LEDGER_TOOL_NAME,
  TaskLedgerStore,
  createTaskLedgerTool,
  emptyTaskLedger,
  isKnownEvidenceRef,
  parseTaskLedgerRecord,
  renderTaskLedgerState,
  taskLedgerRecordBytes,
  taskLedgerRecordSchema,
} from "./tools/task-ledger.js";
export type { ParseTaskLedgerResult, TaskLedgerRecord } from "./tools/task-ledger.js";
export {
  artefactTransformAdvertised,
  extractGradedScalarFromObservation,
  gateArtefactTransform,
  evalTerminalGrade,
  gradedScalarAfterLiveWrites,
  hostMayInjectAppWorldComplete,
  isEvalTerminalTool,
  lastSuccessfulEvalTerminal,
  submittedAnswerFromSubmitAnswer,
  successfulEvalTerminalInStep,
  validSubmitAnswerPayload,
  taskPrefersLabelAnswer,
  writeCountFromSummary,
  COMPLETE_TASK_TOOL_NAME,
  SUBMIT_ANSWER_TOOL_NAME,
} from "./tools/format.js";
export type { EvalTerminalGrade, SuccessfulEvalTerminal } from "./tools/format.js";
export {
  applyArtifactLedger,
  artifactKeysInText,
  artifactRefFromToolInput,
  previewRequiresArtifact,
  runIdFromArtifactRef,
} from "./tools/artifact-contract.js";
export {
  ARTIFACT_IMAGE_PIN_RE,
  pinLocalArtifactImageSync,
  pinnedArtifactImage,
} from "./tools/artifact-process.js";
export { maybeCompactMessages } from "./runtime/compaction.js";

export { defineEval, isEvalDefinition } from "./evals/define-eval.js";
export type { DefineEvalInput, EvalAssert, EvalDefinition, EvalToolName } from "./evals/define-eval.js";
export {
  discoverEvalFiles,
  runAllEvals,
  runEval,
  runEvalFile,
  requireLiveEvalGateway,
} from "./evals/run-eval.js";
export type { EvalRunResult } from "./evals/run-eval.js";

export {
  authoredSlots,
  walkAgentProject,
} from "./discovery/project-walker.js";
export type {
  AuthoredSlot,
  DiscoveredCatalog,
  DiscoveredInstructions,
  DiscoveredNamedFile,
  DiscoveredSubagent,
  DiscoveryDiagnostic,
  ProjectDiscovery,
} from "./discovery/project-walker.js";

export { createDevServer, startDevServer } from "./dev/server.js";
export type { DevServerHandle, DevServerOptions } from "./dev/server.js";

export {
  createPlasmApp,
  handlePlasmRequest,
  handlePlasmOperatorRequest,
  normalizePlasmPathname,
  rewriteRequestPath,
  vercelPlasmHandler,
} from "./server/plasm-handler.js";
export type { PlasmApp, PlasmAppMode, PlasmAppOptions, VercelHandler } from "./server/plasm-handler.js";

export { SymbolRegistry } from "./symbol-registry.js";
export type { SymbolBinding, SymbolKind, SymbolRegistrySnapshot } from "./symbol-registry.js";

export {
  LocalSessionStore,
  SessionManager,
} from "./session-state.js";
export type {
  AgentSessionState,
  ExecuteSessionRef,
  SessionStore,
  TeachingWave,
} from "./session-state.js";

export {
  CatalogManifestSchema,
  FilesystemCatalogLoader,
} from "./catalog/loader.js";
export type {
  CatalogLoader,
  CatalogManifest,
  LoadedCatalog,
} from "./catalog/loader.js";

export { NapiPlasmEngine, createEngine, isNativeEngineAvailable } from "./engine/napi-binding.js";
export { createDefaultHostTransport } from "./engine/host-transport.js";
export type { HostTransportOptions } from "./engine/host-transport.js";
export { createFixtureMockTransport } from "./engine/fixture-mock-transport.js";
export { createProductionHostTransport, createStubHostTransport } from "./engine/create-host-transport.js";
export { loadAgentEnv } from "./load-env.js";
export {
  DEFAULT_EVAL_MAX_OUTPUT_TOKENS,
  resolveEvalMaxOutputTokens,
} from "./eval-max-output-tokens.js";
export {
  isGatewayConfigured,
  isVercelHosted,
  resolveGatewayModel,
} from "./gateway-model.js";
export {
  connectorUidForEntry,
  connectAuthOptionsForEntry,
  resolveConnectBearer,
  ConnectAuthorizationRequiredError,
} from "./engine/connect-auth.js";
export type { ConnectAuthOptions, ConnectTokenSubject } from "./engine/connect-auth.js";
export type {
  DryRunResult,
  HostTransportFn,
  HostTransportRequest,
  HostTransportResponse,
  PlasmEngine,
  ResolvedPlanPayload,
  TeachingExposureResult,
} from "./engine/napi-binding.js";

export {
  PlasmSpanAttributes,
  createAgentTelemetry,
  ensureOtelIntegration,
  registerAgentInstrumentation,
} from "./instrumentation.js";
export type { AgentInstrumentationOptions } from "./instrumentation.js";

export { AgentRuntime } from "./runtime/agent-runtime.js";
export type {
  AgentArchiveStore,
  AgentRuntimeConfig,
  DiscoverInput,
  PlasmContextInput,
  PlasmPlanInput,
  PlasmRunInput,
} from "./runtime/agent-runtime.js";

export { PlasmAgent } from "./runtime/plasm-agent.js";
export type { AgentTurnResult, PlasmAgentConfig } from "./runtime/plasm-agent.js";

export {
  formatLogicalSessionWireRef,
  parseLogicalSessionWireRef,
} from "./runtime/logical-session.js";

export { deriveIntent, intentProvenanceSchema, workflowIntentSchema, logicalSessionRefSchema, sessionIdentitySchema } from "./runtime/session-contract.js";
export type { IntentProvenance, WorkflowIntent, LogicalSessionRef, SessionIdentity } from "./runtime/session-contract.js";

export { createPlasmTools } from "./tools/plasm-tools.js";
export type { PlasmTools } from "./tools/plasm-tools.js";

export {
  TASK_LEDGER_LITURGY,
  TASK_LEDGER_PLAN_OVERLAY,
  TASK_LEDGER_PLAN_SLOT,
  WORKFLOW_COMPLETION_SLOT,
  buildDefaultSystemLiturgy,
  loadPromptAsset,
  overlayNamedSlot,
  overlayWorkflowCompletion,
} from "./prompts/index.js";
export type { PromptAssetName, SystemLiturgyOptions } from "./prompts/index.js";

export {
  DISCOVER_TOOL_DESCRIPTION,
  PLASM_CONTEXT_TOOL_DESCRIPTION,
  PLASM_READ_RUN_ARTIFACT_TOOL_DESCRIPTION,
  PLASM_RUN_TOOL_DESCRIPTION,
  PLASM_TOOL_DESCRIPTION,
} from "./tools/descriptions.js";

export type { PlasmReadRunArtifactInput } from "./runtime/agent-runtime.js";

export { createOperatorRoutes, nitroOperatorHandler } from "./operator/routes.js";
export { renderOperatorShell } from "./operator/ui-shell.js";
export type { NitroHandler, OperatorHandler, OperatorRouteContext } from "./operator/routes.js";

export {
  createAgentStateStore,
  resolveStateBackend,
} from "./state/define-state.js";
export type { AgentStateStore, StateBackend } from "./state/define-state.js";

export {
  bootstrapWorkflowWorld,
  resolveWorkflowWorldType,
} from "./workflow/world-bootstrap.js";
export type { WorkflowWorldType } from "./workflow/world-bootstrap.js";

export {
  LocalArchiveStore,
  resolveArchivePaths,
  UnimplementedBlobArchiveAdapter,
  UnimplementedKvArchiveIndexAdapter,
} from "./archive/index.js";
export { createArchiveStore, resolveArchiveBackend } from "./archive/resolve-backend.js";
export { ProdArchiveStore } from "./archive/prod-archive-store.js";
export { VercelBlobArchiveAdapter } from "./archive/vercel-blob-adapter.js";
export { VercelKvArchiveIndexAdapter } from "./archive/vercel-kv-adapter.js";
export { PostgresArchiveIndexAdapter } from "./archive/postgres-kv-adapter.js";
export type {
  ArchivePaths,
  BlobArchiveAdapter,
  KvArchiveIndexAdapter,
  PlanArchiveSnapshot,
  RunSnapshot,
  TraceDetail,
  TraceRecord,
  TraceSummary,
} from "./archive/index.js";

export { plasmSpans, withPlasmSpan, activeTraceId } from "./telemetry/plasm-spans.js";
export type { PlasmSpanContext } from "./telemetry/plasm-spans.js";
export type {
  OperatorCatalogEntry,
  OperatorCatalogsResponse,
  OperatorHealthResponse,
  OperatorOpsResponse,
  OperatorPlanCommit,
  OperatorPlansResponse,
  OperatorSessionEntry,
  OperatorSessionsResponse,
  OperatorStubFreshness,
} from "./operator/types.js";

export {
  computeCatalogCgsHash,
  generateAllStubs,
  generateStubForCatalog,
  parseCgsDomain,
  parseDomainYaml,
  readStubProvenance,
  renderStubModule,
  stubFreshness,
} from "./stubs/generator.js";
export type {
  ParsedCapability,
  ParsedCgsDomain,
  ParsedDomain,
  ParsedEntity,
  StubGenerationResult,
  StubProvenance,
} from "./stubs/generator.js";

export {
  assignCapabilityBindings,
  assignEntitySymbols,
  stubEntityNames,
} from "./stubs/stub-symbols.js";
export type { CapabilityBinding, EntitySymbolBinding } from "./stubs/stub-symbols.js";

export {
  valueDomainToTsType,
  renderEntityType,
  renderCapabilityInputType,
} from "./stubs/cgs-ts-types.js";

export {
  createCatalogClient,
  dryRunProgram,
  ensureCatalogLoaded,
  ensureStubSession,
  executeRows,
  buildDottedArgs,
  plasmLiteral,
  plasmNumber,
  plasmBoolean,
} from "./stubs/catalog-client.js";
export type {
  CatalogClientOptions,
  ExecuteRowsResult,
  StubInvokeOptions,
} from "./stubs/catalog-client.js";

export { createProgramBuilder } from "./stubs/program-builder.js";
export type {
  ProgramBuilder,
  ProgramBuilderOptions,
  ProgramBuilderProvenance,
} from "./stubs/program-builder.js";
