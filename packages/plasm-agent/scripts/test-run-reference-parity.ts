import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { runReferenceSchema } from "../src/runtime/session-contract.js";
import { formatLogicalSessionWireRef } from "../src/runtime/logical-session.js";
const { isExecutableRunReference } = createRequire(import.meta.url)("@plasm_lang/engine") as {isExecutableRunReference(raw:string):boolean};
const samples = ["", "for", "????", "pc", "pc1 ", " pc1", "pr"+"0".repeat(64), "pg"+"0".repeat(25)];
for (let suffix=0;suffix<4;suffix++) {
  const bytes = Buffer.alloc(16); bytes[15]=suffix;
  const session = formatLogicalSessionWireRef(bytes);
  for (const digits of ["0","1","01","9".repeat(24),"9".repeat(25),"x","-1"]) {
    samples.push(`pc${digits}`,`pg${digits}`,`${session}_pg${digits}`);
  }
  samples.push(`${session.slice(0,-1)}B_pg1`);
}
for (const value of samples) assert.equal(runReferenceSchema.safeParse(value).success,isExecutableRunReference(value),value);
console.log("PASS: TypeScript reference schema agrees with the native shared parser");
