#!/usr/bin/env node
/**
 * Smoke: shape-driven stub programs dry-run against plasm_language_matrix + capability_with_input.
 */
import { mkdtemp, rm } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { createEngine } from "../src/engine/napi-binding.js";
import { dryRunProgram } from "../src/stubs/catalog-client.js";
import { parseCatalogIntrospection } from "../src/stubs/catalog-introspection.js";
import { generateStubFromCatalogDir } from "../src/stubs/generator.js";
import { assignCapabilityBindings, stubEntityNames } from "../src/stubs/stub-symbols.js";
import { createProgramBuilder } from "../src/stubs/program-builder.js";

const packageRoot = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const repoRoot = path.resolve(packageRoot, "../../..");
const matrixDir = path.join(repoRoot, "plasm-oss/fixtures/schemas/plasm_language_matrix");
const capabilityInputDir = path.join(repoRoot, "plasm-oss/fixtures/schemas/capability_with_input");

async function dryRunMatrixPrograms(): Promise<void> {
  const engine = createEngine();
  await engine.loadCatalog({
    rootDir: matrixDir,
    manifest: { entryId: "plasm_language_matrix", label: "plasm_language_matrix" },
  });

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

  const cases: Array<{ name: string; program: string; assertSubstrings: string[] }> = [
    {
      name: "lang_effect_create_literal",
      program: `${sym}.create(title="MatrixCreated", score=7, owner="bot")`,
      assertSubstrings: [".create(", "title=", "score=", "owner="],
    },
    {
      name: "lang_effect_update",
      program: `${sym}("i1").update(title="MatrixPatch", score=42, owner="alice")`,
      assertSubstrings: [".update(", "title=", "score="],
    },
    {
      name: "lang_effect_action_ping",
      program: `${sym}("i1").ping()`,
      assertSubstrings: [".ping()"],
    },
    {
      name: "lang_effect_delete",
      program: `${sym}("i2").delete()`,
      assertSubstrings: [".delete()"],
    },
  ];

  for (const { name, program, assertSubstrings } of cases) {
    for (const sub of assertSubstrings) {
      if (!program.includes(sub)) {
        throw new Error(`${name}: program missing ${JSON.stringify(sub)}: ${program}`);
      }
    }
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
    if (!src.includes(".update(") || !src.includes("buildDottedArgs") || !src.includes("RefAccount")) {
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
    if (!src.includes(".create(") || !src.includes("RefLangItem")) {
      throw new Error("matrix stub missing shape-driven emission or branded refs");
    }
    console.log("generated matrix stub:", result.outPath);
  } finally {
    await rm(outDir, { recursive: true, force: true });
  }
}

async function main(): Promise<void> {
  await dryRunMatrixPrograms();
  await assertCapabilityWithInputStub();
  await generateMatrixStubFile();
  console.log("\nOK: stub matrix conformance");
}

main().catch((err: unknown) => {
  console.error(err);
  process.exit(1);
});
