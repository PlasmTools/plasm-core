#!/usr/bin/env node
import { loadPackedCatalog } from "../src/catalog/loader.js";
/**
 * Smoke: shape-driven stub programs dry-run against plasm_language_matrix + capability_with_input.
 *
 * Dry-run + substring checks here are secondary evidence only. Authoritative invoke IO conformance
 * lives in `plasm-e2e/tests/plasm_language_matrix_invoke.rs` and `plasm_language_matrix` live runs.
 */
import { mkdtemp, rm } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { createEngine } from "../src/engine/napi-binding.js";
import ts from "typescript";
import { renderProgramStatements } from "../src/stubs/plasm-value-emitter.js";
import { plasmLiteral, buildDottedArgs } from "../src/stubs/catalog-client.js";
import { dryRunProgram } from "../src/stubs/catalog-client.js";
import { parseCatalogIntrospection } from "../src/stubs/catalog-introspection.js";
import { generateStubFromCatalogDir } from "../src/stubs/generator.js";
import { assignCapabilityBindings, stubEntityNames } from "../src/stubs/stub-symbols.js";
import { createProgramBuilder } from "../src/stubs/program-builder.js";

const packageRoot = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const repoRoot = path.resolve(packageRoot, "../../..");
function requiredManifest(variable: string): string {
  const value = process.env[variable];
  if (!value) throw new Error(`${variable} must identify the packed abstract fixture manifest`);
  return value;
}
const matrixDir = requiredManifest("PLASM_MATRIX_MANIFEST");
const capabilityInputDir = requiredManifest("PLASM_CAPABILITY_INPUT_MANIFEST");

async function dryRunMatrixPrograms(): Promise<void> {
  const engine = createEngine();
  await engine.loadCatalog(await loadPackedCatalog(matrixDir));

  const catalog = parseCatalogIntrospection(
    await engine.introspectCatalog("plasm_language_matrix"),
  );
  const bindings = assignCapabilityBindings(catalog);
  const langBinding = bindings.get("langitem_create");
  if (!langBinding) throw new Error("langitem_create binding missing");
  const sym = langBinding.entitySymbol;

  const builder = createProgramBuilder({
    entryId: catalog.entry_id,
    cgsHash: catalog.catalog_cgs_hash,
    catalogRoot: matrixDir,
    stubEntities: stubEntityNames(catalog),
    engine,
  });

  const cases: Array<[string, Record<string, unknown>]> = [
    ["langitem_create", {title: "MatrixCreated", score: 7, owner: "bot"}],
    ["langitem_update", {id: "i1", title: "MatrixPatch", score: 42, owner: "alice"}],
    ["langitem_ping", {id: "i1"}],
    ["langitem_delete", {id: "i2"}],
  ];
  for (const [name, input] of cases) {
    const cap = catalog.capabilities.find(cap => cap.name === name)!;
    const binding = bindings.get(name)!;
    const code = renderProgramStatements(binding, cap, catalog.values, binding.invokeShape, "id", "input");
    const program: string = new Function("input", "plasmLiteral", "buildDottedArgs", ts.transpile(`${code}\nreturn program;`))(input, plasmLiteral, buildDottedArgs);
    const dry = await dryRunProgram(builder, program);
    if (!/^pc\d+$/i.test(dry.planCommitRef)) {
      throw new Error(`${name}: unexpected plan_commit_ref ${dry.planCommitRef}`);
    }
    console.log(`${name}:`, program, "→", dry.planCommitRef);
  }
}

async function assertCapabilityWithInputStub(): Promise<void> {
  const outDir = await mkdtemp(path.join(os.tmpdir(), "plasm-cap-input-stubs-"));
  try {
    const result = await generateStubFromCatalogDir(capabilityInputDir, outDir);
    const { readFile } = await import("node:fs/promises");
    const src = await readFile(result.outPath, "utf8");
    if (!src.includes("account_update")) {
      throw new Error("capability_with_input stub missing account_update");
    }
    if (!src.includes("class Invoke(Program)") || !src.includes("buildDottedArgs") || !src.includes("RefAccount")) {
      throw new Error("capability_with_input stub missing scoped update emission or branded refs");
    }
    console.log("capability_with_input stub:", result.outPath);
  } finally {
    await rm(outDir, { recursive: true, force: true });
  }
}

async function generateMatrixStubFile(): Promise<void> {
  const outDir = await mkdtemp(path.join(os.tmpdir(), "plasm-matrix-stubs-"));
  try {
    const result = await generateStubFromCatalogDir(matrixDir, outDir);
    const { readFile } = await import("node:fs/promises");
    const src = await readFile(result.outPath, "utf8");
    if (!src.includes("langitem_create")) {
      throw new Error("generated matrix stub missing langitem_create");
    }
    if (!src.includes("class Invoke(Program)") || !src.includes("RefLangItem")) {
      throw new Error("matrix stub missing shape-driven emission or branded refs");
    }
    console.log("generated matrix stub:", result.outPath);
  } finally {
    await rm(outDir, { recursive: true, force: true });
  }
}

async function dryRunUnionPrograms(): Promise<void> {
  const manifest = requiredManifest("PLASM_UNION_MANIFEST");
  const engine = createEngine();
  await engine.loadCatalog(await loadPackedCatalog(manifest));
  const catalog = parseCatalogIntrospection(await engine.introspectCatalog("python_union_matrix"));
  const bindings = assignCapabilityBindings(catalog);
  const builder = createProgramBuilder({
    entryId: catalog.entry_id, cgsHash: catalog.catalog_cgs_hash,
    catalogRoot: manifest, stubEntities: stubEntityNames(catalog), engine,
  });
  const cases: Array<[string, Record<string, unknown>, boolean]> = [
    ["record_write", {id: "r1", tenant: "t1", request_id: "req1", kind: "text", text: "hello"}, true],
    ["record_write", {id: "r1", tenant: "t1", request_id: "req1", kind: "count", count: 2, labels: ["red"]}, true],
    ["record_batch", {id: "r1", operations: [{kind: "text", text: "hello"}, {kind: "count", count: 2, labels: ["blue"]}]}, true],
    ["record_write", {id: "r1", tenant: "t1", request_id: "req1", kind: "text", text: "hello", count: 2}, false],
  ];
  for (const [name, input, valid] of cases) {
    const cap = catalog.capabilities.find(cap => cap.name === name)!;
    const binding = bindings.get(name)!;
    const code = renderProgramStatements(binding, cap, catalog.values, binding.invokeShape, "id", "input");
    const program: string = new Function("input", "plasmLiteral", "buildDottedArgs", ts.transpile(`${code}\nreturn program;`))(input, plasmLiteral, buildDottedArgs);
    let error: unknown;
    try { await dryRunProgram(builder, program); } catch (caught) { error = caught; }
    if (valid && error) throw error;
    if (!valid && !error) throw new Error("generated client accepted mixed union variants");
  }
  console.log("Tagged unions: both variants, nested arrays and mixed-field rejection passed through native Python admission");
}

async function main(): Promise<void> {
  await dryRunMatrixPrograms();
  await dryRunUnionPrograms();
  await assertCapabilityWithInputStub();
  await generateMatrixStubFile();
  console.log("\nOK: stub matrix conformance");
}

main().catch((err: unknown) => {
  console.error(err);
  process.exit(1);
});
