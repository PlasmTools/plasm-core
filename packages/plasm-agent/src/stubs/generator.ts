import { createHash } from "node:crypto";
import { mkdir, readFile, stat, writeFile } from "node:fs/promises";
import path from "node:path";

import { createEngine, type PlasmEngine } from "../engine/napi-binding.js";
import { parseCatalogIntrospection, type CatalogIntrospectionJson } from "./catalog-introspection.js";
import {
  capabilityNeedsInput,
  capabilityReturnsScalar,
  capabilityReturnsVoid,
  classifyInvokeShape,
} from "./capability-invoke-shape.js";
import {
  capabilityInputTypeName,
  renderBrandTypes,
  renderCapabilityInputType,
  renderEntityRowType,
} from "./input-type-to-ts.js";
import { renderProgramStatements } from "./plasm-value-emitter.js";
import { parseCgsDomain, toLegacyParsedDomain, type ParsedCgsDomain } from "./domain-parser.js";
import {
  assignCapabilityBindings,
  capabilityReturnTypeName,
  stubEntityNames,
  type CapabilityBinding,
} from "./stub-symbols.js";

/** @deprecated Use {@link ParsedCgsDomain} from domain-parser. */
export interface ParsedEntity {
  name: string;
}

/** @deprecated Use {@link ParsedCgsDomain} from domain-parser. */
export interface ParsedCapability {
  name: string;
  entity: string;
  kind?: string;
}

/** @deprecated Use {@link ParsedCgsDomain} from domain-parser. */
export interface ParsedDomain {
  entryId: string;
  authScheme?: string;
  entities: ParsedEntity[];
  capabilities: ParsedCapability[];
}

export interface StubGenerationResult {
  entryId: string;
  catalogCgsHash: string;
  outPath: string;
  generatedAt: string;
}

const PROVENANCE_RE =
  /@generated catalog_cgs_hash=([a-f0-9]+) entry_id=([^\s]+)/;

export function computeCatalogCgsHash(domainYaml: string, mappingsYaml: string): string {
  return createHash("sha256").update(domainYaml).update("\n").update(mappingsYaml).digest("hex");
}

/** Legacy parser — delegates to {@link parseCgsDomain}. */
export function parseDomainYaml(raw: string, fallbackEntryId: string): ParsedDomain {
  return toLegacyParsedDomain(parseCgsDomain(raw, fallbackEntryId));
}

function toExportName(entryId: string): string {
  const safe = entryId.replace(/[^a-zA-Z0-9_]/g, "_");
  return /^[a-zA-Z_]/.test(safe) ? safe : `_${safe}`;
}

function indent(text: string, spaces: number): string {
  const pad = " ".repeat(spaces);
  return text
    .split("\n")
    .map((line) => (line ? pad + line : line))
    .join("\n");
}

function entityByName(catalog: CatalogIntrospectionJson, name: string) {
  return catalog.entities.find((e) => e.name === name);
}

function renderCapabilityFunction(
  catalog: CatalogIntrospectionJson,
  binding: CapabilityBinding | undefined,
): string {
  const cap = catalog.capabilities.find((c) => c.name === binding?.capability);
  if (!cap || !binding) {
    const name = binding?.capability ?? "unknown";
    return `export async function ${name}(): Promise<unknown> {
  throw new Error(${JSON.stringify(`${name}: unresolved capability binding`)});
}`;
  }

  const entity = entityByName(catalog, cap.entity);
  if (!entity) {
    return `export async function ${cap.name}(): Promise<unknown> {
  throw new Error(${JSON.stringify(`${cap.name}: unknown entity ${cap.entity}`)});
}`;
  }

  const shape = binding.invokeShape;
  const returnType = capabilityReturnTypeName(cap.entity);
  const scalar = capabilityReturnsScalar(cap);
  const voidReturn = capabilityReturnsVoid(cap);
  const needsInput = capabilityNeedsInput(cap, shape, entity.id_field);
  const inputType = capabilityInputTypeName(cap.name);
  const optionsArg = "options?: StubInvokeOptions";
  const params = needsInput ? `input: ${inputType}, ${optionsArg}` : optionsArg;

  const returnTypeTs = voidReturn ? "void" : scalar ? returnType : `${returnType}[]`;
  const programStmt = renderProgramStatements(
    binding,
    cap,
    catalog.values,
    shape,
    entity.id_field,
    needsInput ? "input" : "input",
  );

  let programBody: string;
  if (voidReturn) {
    programBody = `${programStmt}
  await executeRows(builder, program, options);`;
  } else if (needsInput) {
    programBody = `${programStmt}
  const result = await executeRows<${returnType}>(builder, program, options);`;
  } else {
    programBody = `${programStmt}
  const result = await executeRows<${returnType}>(builder, program, options);`;
  }

  let returnStmt = "";
  if (!voidReturn) {
    returnStmt = scalar
      ? `const row = result.rows[0];
  if (!row) throw new Error(${JSON.stringify(`${cap.name}: empty result`)});
  return row;`
      : "return result.rows;";
  }

  const desc = cap.input_schema?.description ?? cap.name;
  return `/** ${desc} */
export async function ${cap.name}(${params}): Promise<${returnTypeTs}> {
  ${programBody}
  ${returnStmt}
}`;
}

export function renderStubModule(
  catalog: CatalogIntrospectionJson,
  generatedAt: string,
  bindings?: Map<string, CapabilityBinding>,
): string {
  const exportName = toExportName(catalog.entry_id);
  const clientImport = "@plasm_lang/vercel-agent";

  const entityTypes = new Set<string>();
  const inputTypes = new Set<string>();

  for (const entity of catalog.entities) {
    const caps = catalog.capabilities.filter((c) => c.entity === entity.name);
    const fieldNames = new Set<string>();
    for (const cap of caps) {
      for (const field of cap.provides) {
        fieldNames.add(field);
      }
    }
    if (fieldNames.size) {
      entityTypes.add(renderEntityRowType(entity.name, [...fieldNames], catalog));
    }
  }

  for (const cap of catalog.capabilities) {
    const shape = bindings?.get(cap.name)?.invokeShape ?? classifyInvokeShape(cap);
    const inputType = renderCapabilityInputType(cap, catalog, shape);
    if (inputType) inputTypes.add(inputType);
  }

  const capabilityFns = catalog.capabilities.map((cap) =>
    renderCapabilityFunction(catalog, bindings?.get(cap.name)),
  );

  const capsByEntity = new Map<string, typeof catalog.capabilities>();
  for (const cap of catalog.capabilities) {
    const list = capsByEntity.get(cap.entity) ?? [];
    list.push(cap);
    capsByEntity.set(cap.entity, list);
  }

  const namespaceBlocks: string[] = [];
  for (const entity of catalog.entities) {
    const caps = capsByEntity.get(entity.name) ?? [];
    if (!caps.length) continue;
    const entries = caps.map((c) => `${c.name}`).join(",\n");
    namespaceBlocks.push(`${entity.name}: {\n${indent(entries, 4)},\n  }`);
  }

  const stubEntities = stubEntityNames(catalog);

  return `/** @generated catalog_cgs_hash=${catalog.catalog_cgs_hash} entry_id=${catalog.entry_id} generated_at=${generatedAt} */
import path from "node:path";
import { fileURLToPath } from "node:url";

import {
  createCatalogClient,
  buildDottedArgs,
  executeRows,
  plasmBoolean,
  plasmLiteral,
  plasmNumber,
  type StubInvokeOptions,
} from ${JSON.stringify(clientImport)};

export const catalogCgsHash = ${JSON.stringify(catalog.catalog_cgs_hash)};
export const entryId = ${JSON.stringify(catalog.entry_id)};

const catalogRoot = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "../../catalogs/${catalog.entry_id}",
);

const stubEntities = ${JSON.stringify(stubEntities)} as const;

const builder = createCatalogClient({
  entryId,
  cgsHash: catalogCgsHash,
  catalogRoot,
  stubEntities: [...stubEntities],
});

${renderBrandTypes(catalog)}

${[...entityTypes, ...inputTypes].join("\n\n")}

${capabilityFns.join("\n\n")}

export const ${exportName} = {
${indent(namespaceBlocks.join(",\n"), 2)},
  builder,
  catalogCgsHash,
  entryId,
} as const;

export default ${exportName};
`;
}

export async function generateStubForCatalog(
  catalogDir: string,
  outDir: string,
  engine?: PlasmEngine,
): Promise<StubGenerationResult> {
  const fallbackEntryId = path.basename(catalogDir);
  const domainYaml = await readFile(path.join(catalogDir, "domain.yaml"), "utf8");
  const domainMeta = parseCgsDomain(domainYaml, fallbackEntryId);
  const generatedAt = new Date().toISOString();

  const activeEngine = engine ?? createEngine();
  await activeEngine.loadCatalog({
    rootDir: catalogDir,
    manifest: { entryId: domainMeta.entryId, label: fallbackEntryId },
  });

  const raw = await activeEngine.introspectCatalog(domainMeta.entryId);
  const catalog = parseCatalogIntrospection(raw);
  const bindings = assignCapabilityBindings(catalog);
  const source = renderStubModule(catalog, generatedAt, bindings);

  await mkdir(outDir, { recursive: true });
  const outPath = path.join(outDir, `${catalog.entry_id}.ts`);
  await writeFile(outPath, source, "utf8");

  return {
    entryId: catalog.entry_id,
    catalogCgsHash: catalog.catalog_cgs_hash,
    outPath,
    generatedAt,
  };
}

export async function generateAllStubs(
  agentRoot: string,
  options?: { engine?: PlasmEngine },
): Promise<StubGenerationResult[]> {
  const catalogsDir = path.join(agentRoot, "catalogs");
  const outDir = path.join(agentRoot, ".plasm", "stubs");
  const { access, readdir, stat } = await import("node:fs/promises");

  const engine = options?.engine ?? createEngine();

  let entries;
  try {
    entries = await readdir(catalogsDir, { withFileTypes: true });
  } catch {
    return [];
  }

  const results: StubGenerationResult[] = [];
  for (const entry of entries) {
    if (!entry.isDirectory() && !entry.isSymbolicLink()) continue;
    const rootDir = path.join(catalogsDir, entry.name);
    try {
      const info = await stat(rootDir);
      if (!info.isDirectory()) continue;
      await access(path.join(rootDir, "domain.yaml"));
      await access(path.join(rootDir, "mappings.yaml"));
    } catch {
      continue;
    }
    results.push(await generateStubForCatalog(rootDir, outDir, engine));
  }
  results.sort((a, b) => a.entryId.localeCompare(b.entryId));
  return results;
}

/** Generate stub from an arbitrary catalog directory (fixtures / matrix smoke). */
export async function generateStubFromCatalogDir(
  catalogDir: string,
  outDir: string,
  engine?: PlasmEngine,
): Promise<StubGenerationResult> {
  return generateStubForCatalog(catalogDir, outDir, engine);
}

export interface StubProvenance {
  catalogCgsHash: string;
  entryId: string;
  generatedAt?: string;
}

export async function readStubProvenance(stubPath: string): Promise<StubProvenance | null> {
  try {
    const raw = await readFile(stubPath, "utf8");
    const match = raw.match(PROVENANCE_RE);
    if (!match) return null;
    const generatedMatch = raw.match(/generated_at=([^\s*]+)/);
    return {
      catalogCgsHash: match[1],
      entryId: match[2],
      generatedAt: generatedMatch?.[1],
    };
  } catch {
    return null;
  }
}

export async function stubFreshness(
  liveHash: string,
  stubPath: string,
): Promise<{
  stubPath: string;
  liveCatalogCgsHash: string;
  generatedCatalogCgsHash: string | null;
  fresh: boolean;
  lastBuiltAt: string | null;
  validationErrors: string[];
}> {
  const validationErrors: string[] = [];
  let lastBuiltAt: string | null = null;
  let generatedCatalogCgsHash: string | null = null;

  try {
    const info = await stat(stubPath);
    lastBuiltAt = info.mtime.toISOString();
  } catch {
    validationErrors.push("stub file missing");
  }

  const provenance = await readStubProvenance(stubPath);
  if (!provenance) {
    validationErrors.push("stub provenance comment missing or invalid");
  } else {
    generatedCatalogCgsHash = provenance.catalogCgsHash;
  }

  const fresh = generatedCatalogCgsHash === liveHash && validationErrors.length === 0;
  return {
    stubPath,
    liveCatalogCgsHash: liveHash,
    generatedCatalogCgsHash,
    fresh,
    lastBuiltAt,
    validationErrors,
  };
}

export type { ParsedCgsDomain } from "./domain-parser.js";
export { parseCgsDomain } from "./domain-parser.js";
