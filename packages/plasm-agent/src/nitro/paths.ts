import path from "node:path";

export const PLASM_NITRO_BUILD_DIR = ".plasm/nitro";
export const PLASM_NITRO_ROUTES_DIR = ".plasm/nitro/routes";
export const PLASM_NITRO_OUTPUT_DIR = ".plasm/nitro-output";
export const PLASM_AGENT_SUMMARY_PATH = ".plasm/agent-summary.json";
export const EVE_AGENT_SUMMARY_PATH = ".eve/agent-summary.json";

export function plasmNitroBuildDir(projectRoot: string): string {
  return path.join(projectRoot, PLASM_NITRO_BUILD_DIR);
}

export function plasmNitroRoutesDir(projectRoot: string): string {
  return path.join(projectRoot, PLASM_NITRO_ROUTES_DIR);
}

export function plasmNitroOutputDir(projectRoot: string): string {
  return path.join(projectRoot, PLASM_NITRO_OUTPUT_DIR);
}

export function plasmAgentSummaryPath(projectRoot: string): string {
  return path.join(projectRoot, PLASM_AGENT_SUMMARY_PATH);
}

export function eveAgentSummaryPath(projectRoot: string): string {
  return path.join(projectRoot, EVE_AGENT_SUMMARY_PATH);
}

export function vercelOutputDir(projectRoot: string): string {
  return path.join(projectRoot, ".vercel", "output");
}

export function isVercelBuildEnvironment(): boolean {
  return process.env.VERCEL === "1" || process.env.VERCEL === "true";
}
