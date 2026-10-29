import assert from "node:assert/strict";
import { formatRecordedOutputs, runOutputArtifactSchema } from "../src/tools/recorded-outputs.js";

const id = `pr${"a".repeat(64)}`;
const snapshot = (entry: string, rows: Array<Record<string, unknown>>, materialization = "Catalog") => ({
  entry_id: entry, entity: "Resource", row_materialization: materialization,
  entities: rows.map(fields => ({ completeness: "complete", fields })),
});
// Abstract matrix: empty/single/plural, federated same-name types, distinct
// producer bindings, missing versus null, computed outputs, absent metadata.
for (const entry of ["alpha", "beta"]) {
  for (const binding of ["acquired", "verified", "branch.inner"]) {
    for (const rows of [[], [{ key: "SECRET", optional: null }], [{key: "SECRET", optional: null}, {key: "SECRET"}]]) {
      const artifact = runOutputArtifactSchema.parse({run_id: id, producer_binding: binding, reusable: true, snapshot: snapshot(entry, rows)});
      const output = formatRecordedOutputs([artifact]);
      assert.ok(output.includes(`${binding}: ${entry}::Resource;`));
      assert.ok(!output.includes('Resource[]'), 'observed rows do not establish a list-valued compute input');
      assert.ok(output.includes(id));
      assert.ok(output.includes(`self.recorded("${id}")`));
      assert.ok(!output.includes("SECRET") && !output.includes("null"));
      if (rows.length === 2) assert.match(output, /optional \(some rows\)/);
      if (rows.length === 1) assert.match(output, /observed fields: key, optional$/);
      if (rows.length === 0) assert.match(output, /0 rows; observed fields: none$/);
      assert.ok(output.length < 260, "one compact metadata line, independent of value size");
      const before = JSON.stringify(artifact);
      formatRecordedOutputs([artifact]);
      assert.equal(JSON.stringify(artifact), before, "presentation does not mutate retained output");
    }
  }
}
assert.match(formatRecordedOutputs([{run_id:id,producer_binding:"derived",reusable:false,snapshot:snapshot("alpha",[{key:1}],"Computed")}]), /computed output \(not importable\)/);
assert.match(formatRecordedOutputs([{run_id:id,producer_binding:null,reusable:false,snapshot:{}}]), /binding unavailable; output schema unavailable/);
assert.equal(formatRecordedOutputs([]), "");
assert.match(formatRecordedOutputs([{run_id:id, producer_binding:"partial", reusable:false, snapshot:{
  ...snapshot("alpha", []), entities:[{completeness:"partial",fields:{key:1}}],
}}]), /incomplete rows/);
assert.equal(runOutputArtifactSchema.safeParse({run_id:id,snapshot:{}}).success,false, "native binding metadata is mandatory, not guessed");
for (const rows of [[], [{key:1}]]) {
  const artifact = {run_id:id, producer_binding:"intermediate", reusable:false, snapshot:snapshot("alpha",rows)};
  assert.ok(!formatRecordedOutputs([artifact]).includes("self.recorded("), "row completeness does not imply import eligibility");
}
assert.equal(runOutputArtifactSchema.safeParse({run_id:id,producer_binding:"x",snapshot:{}}).success,false);
console.log("PASS: 18 abstract output-label combinations; no values, replay or invented bindings");
