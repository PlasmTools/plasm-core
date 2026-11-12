# Native discovery starvation suite v2

**Archived invalid fixture version.** Source declared two Gets without a primary
read. Use `../starvation-v2.1/run.py` for the corrected, separately frozen suite.
This version's fixture lock and failed logs remain evidence; its runner refuses
new baseline runs so it cannot accidentally use the newer native adapter.

Extends the unchanged v1 regression suite with 24 frozen growth/path cases.
This is development evidence, not an unseen acceptance benchmark.

```sh
# From the parent monorepo (omit plasm-oss/ in the OSS checkout):
python3 plasm-oss/fixtures/discovery/starvation-v2/run.py verify
python3 plasm-oss/fixtures/discovery/starvation-v2/run.py run /absolute/new/output-directory
python3 plasm-oss/fixtures/discovery/starvation-v2/run.py summary /absolute/output-directory
```

The runner builds the native test once, then runs cases serially in isolated
process groups. Each case includes both insertion orders and four search turns.
The 180-second local deadline starts after compilation. Only the owned test
process group is stopped on timeout; no services, caches or external executions
are touched. Do not increase the deadline or shrink fixtures after seeing scores.

`LOCK.json` protects labels, requests, noise profiles, schemas and the authoring
generator. `MANIFEST.json` pins the native sources and harness for each run.
`TRACE.jsonl` checkpoints indexing, ranks, selection and completed searches even
when a large case times out. `RESULTS.json` contains exact packets and receipts.
`STATUS.json` distinguishes completed, local timeout and native fault.

Outcome coverage means at least one **complete alternative path** for that
outcome. Supplier recall is a separate metric. Both independent outcomes remain
visible. Lexical/frontier ranks, actual native deferral reasons and standalone
target packet costs distinguish retrieval from allocation pressure; no one field
proves a unique causal explanation. Fixture decisions do not measure model quality.

No provider calls or AppWorld execution. Production ranking, authorization,
contracts, 16,000-byte packet limit and one-logical-request limit are unchanged.
