import { register } from "node:module";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const root = path.dirname(fileURLToPath(import.meta.url));
register(pathToFileURL(path.join(root, "resolve-ts-extension.mjs")));
