import { frameworkPackageVersion } from "../package-version.js";

export function createPlasmVercelOptions(enabled: boolean):
  | {
      config: {
        version: 3;
        framework: { version: string };
      };
    }
  | undefined {
  if (!enabled) return undefined;
  return {
    config: {
      version: 3,
      framework: { version: frameworkPackageVersion() },
    },
  };
}
