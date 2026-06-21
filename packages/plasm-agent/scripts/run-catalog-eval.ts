#!/usr/bin/env node
/**
 * Bridge to plasm-eval: runs catalog eval cases.yaml against OpenRouter + BAML.
 */
import { spawnSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";

const packageRoot = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const repoRoot = path.resolve(packageRoot, "../../..");

function parseArgs(argv: string[]): Record<string, string> {
  const out: Record<string, string> = {};
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i]!;
    if (!arg.startsWith("--")) continue;
    const key = arg.slice(2);
    const next = argv[i + 1];
    if (next && !next.startsWith("--")) {
      out[key] = next;
      i += 1;
    } else {
      out[key] = "true";
    }
  }
  return out;
}

async function main(): Promise<void> {
  const args = parseArgs(process.argv.slice(2));
  const schema = args.schema ?? "fixtures/schemas/pokeapi_mini";
  const cases =
    args.cases ?? path.join(repoRoot, schema, "eval/cases.yaml");
  const model = args.model ?? process.env.PLASM_EVAL_MODEL ?? "anthropic/claude-sonnet-4.6";
  const subcommand = args.coverage === "true" ? "coverage" : null;

  const cargoArgs = ["run", "-p", "plasm-eval", "--"];
  if (subcommand) {
    cargoArgs.push("coverage", "--schema", schema, "--cases", cases, "--format", "json");
  } else {
    cargoArgs.push("--schema", schema, "--cases", cases, "--model", model);
    if (args.attempts) cargoArgs.push("--attempts", args.attempts);
    if (args["report-dir"]) cargoArgs.push("--report-dir", args["report-dir"]);
  }

  console.log("[eval:catalog]", cargoArgs.join(" "));
  const result = spawnSync("cargo", cargoArgs, {
    cwd: repoRoot,
    stdio: "inherit",
    env: process.env,
  });

  if (result.status !== 0) {
    process.exit(result.status ?? 1);
  }
}

main().catch((err: unknown) => {
  console.error(err);
  process.exit(1);
});
