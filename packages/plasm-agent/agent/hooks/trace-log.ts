import { defineHook } from "../../src/authoring/define-hook.js";

export default defineHook({
  name: "trace-log",
  on: ["run:complete", "plan:commit"],
  handler: (_ctx, detail) => {
    const event = detail?.planCommitRef ? "plan:commit" : "run:complete";
    console.log(`[plasm:hook:trace-log] ${event}`, detail ?? {});
  },
});
