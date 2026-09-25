import { defineSchedule } from "../../src/authoring/define-schedule.js";
import { dryRunProgram } from "../../src/stubs/catalog-client.js";

import executeTiny from "../.plasm/stubs/execute_tiny.js";

export default defineSchedule({
  name: "ping",
  cron: "*/5 * * * *",
  handler: async () => {
    const dry = await dryRunProgram(executeTiny.builder, "class Products(Program):\n    def build(self):\n        return e2.query()");
    console.log(`[plasm:schedule:ping] product_list dry-run ${dry.planCommitRef}`);
  },
});
