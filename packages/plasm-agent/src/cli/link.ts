import { spawn } from "node:child_process";

function runCommand(
  command: string,
  args: string[],
  cwd: string,
): Promise<{ code: number; stdout: string; stderr: string }> {
  return new Promise((resolve) => {
    const child = spawn(command, args, {
      cwd,
      stdio: ["ignore", "pipe", "pipe"],
      env: process.env,
    });
    let stdout = "";
    let stderr = "";
    child.stdout?.on("data", (chunk) => {
      stdout += chunk;
    });
    child.stderr?.on("data", (chunk) => {
      stderr += chunk;
    });
    child.on("close", (code) => {
      resolve({ code: code ?? 1, stdout, stderr });
    });
  });
}

async function commandExists(command: string): Promise<boolean> {
  const probe = process.platform === "win32" ? "where" : "which";
  const result = await runCommand(probe, [command], process.cwd());
  return result.code === 0;
}

export interface PlasmLinkResult {
  linked: boolean;
  envPulled: boolean;
  messages: string[];
}

export async function runPlasmLink(projectRoot: string): Promise<PlasmLinkResult> {
  const messages: string[] = [];
  const hasVercel = await commandExists("vercel");
  if (!hasVercel) {
    messages.push(
      "Vercel CLI not found. Install with `npm i -g vercel` or set AI_GATEWAY_API_KEY manually.",
    );
    return { linked: false, envPulled: false, messages };
  }

  messages.push("Running `vercel link` (interactive)…");
  const link = await runCommand("vercel", ["link"], projectRoot);
  if (link.code !== 0) {
    messages.push("vercel link failed or was cancelled.");
    if (link.stderr.trim()) messages.push(link.stderr.trim());
    return { linked: false, envPulled: false, messages };
  }
  messages.push("vercel link complete.");

  messages.push("Running `vercel env pull .env.local`…");
  const pull = await runCommand("vercel", ["env", "pull", ".env.local"], projectRoot);
  if (pull.code !== 0) {
    messages.push("vercel env pull failed — set AI_GATEWAY_API_KEY in .env.local manually.");
    if (pull.stderr.trim()) messages.push(pull.stderr.trim());
    return { linked: true, envPulled: false, messages };
  }
  messages.push("Pulled environment to .env.local");
  return { linked: true, envPulled: true, messages };
}
