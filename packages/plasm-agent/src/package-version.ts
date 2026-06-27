import plasmAgentPackage from "../package.json" with { type: "json" };

/** Published semver from @plasm_lang/vercel-agent package.json (inlined when Nitro bundles). */
export function frameworkPackageVersion(): string {
  return plasmAgentPackage.version;
}
