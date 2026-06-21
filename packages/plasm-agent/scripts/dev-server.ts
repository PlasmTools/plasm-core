#!/usr/bin/env node
/**
 * Local dev server with hot reload for catalogs/, skills/, channels/, schedules/, hooks/, and instructions.
 *
 *   npm run dev
 *   curl http://127.0.0.1:3000/plasm/v1/info
 *   curl -X POST http://127.0.0.1:3000/plasm/v1/session \
 *     -H 'content-type: application/json' \
 *     -d '{"message":"list products"}'
 */
import path from "node:path";
import { fileURLToPath } from "node:url";

import {
  installDevServerShutdown,
  startDevServerForProject,
} from "../src/cli/dev.js";

const packageRoot = path.dirname(path.dirname(fileURLToPath(import.meta.url)));

const handle = await startDevServerForProject({
  project: {
    projectRoot: packageRoot,
    agentRoot: path.join(packageRoot, "agent"),
  },
});

installDevServerShutdown(handle);
