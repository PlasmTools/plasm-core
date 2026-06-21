/**
 * Resolve `.js` import specifiers to sibling `.ts` sources for no-build dev runs.
 */
export async function resolve(specifier, context, nextResolve) {
  try {
    return await nextResolve(specifier, context);
  } catch (error) {
    if (typeof specifier === "string" && specifier.endsWith(".js")) {
      const tsSpecifier = `${specifier.slice(0, -3)}.ts`;
      try {
        return await nextResolve(tsSpecifier, context);
      } catch {
        // fall through
      }
    }
    throw error;
  }
}
