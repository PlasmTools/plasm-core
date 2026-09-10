/**
 * MCP-aligned tool descriptions — loaded from plasm-core prompt assets
 * (see `src/prompts/`). Do not hand-fork grammar here.
 */
import {
  buildDiscoverToolDescription,
  buildPlasmContextToolDescription,
  buildPlasmReadRunArtifactToolDescription,
  buildPlasmRunToolDescription,
  buildPlasmToolDescription,
} from "../prompts/index.js";

export const DISCOVER_TOOL_DESCRIPTION = buildDiscoverToolDescription();
export const PLASM_CONTEXT_TOOL_DESCRIPTION = buildPlasmContextToolDescription();
export const PLASM_TOOL_DESCRIPTION = buildPlasmToolDescription();
export const PLASM_RUN_TOOL_DESCRIPTION = buildPlasmRunToolDescription();
export const PLASM_READ_RUN_ARTIFACT_TOOL_DESCRIPTION =
  buildPlasmReadRunArtifactToolDescription();
