# Resource breadth and continuation controls

The protocol, cases, growth profiles, generator and inherited schema were frozen
before selection measurement. No production policy changed. These are development
controls; they are not a holdout or an agent evaluation.

From the OSS root:

```sh
python3 fixtures/discovery/resource-breadth-v5/run.py verify
python3 fixtures/discovery/resource-breadth-v5/run.py run eval-reports/<fresh-directory>
python3 fixtures/discovery/resource-breadth-v5/run.py quality eval-reports/<fresh-directory>/RESULTS.json
python3 fixtures/discovery/resource-breadth-v5/service-proof.py eval-reports/<fresh-proof-directory>
```

The service runner creates and removes only its uniquely named disposable pgvector
container. It uses the installed Docker image and fresh native test binary; no model
calls or task execution occur. It runs all seven ignored service ownership proofs,
not only the new continuation check. Exact source hashes and logs are retained.

Use `trajectory-v4/compare.py` on two runs of this same frozen cohort. Profile names
are part of case identity, so concentrated and distributed rows cannot overwrite one
another. The shared runner's report heading retains its original v4 name; manifests,
case IDs and this directory identify the v5 cohort. Raw results are authoritative.

## Baseline findings

Evidence: `eval-reports/resource-breadth-v5-baseline-r2/`. All **32/32** variants meet
the frozen four-search gate. All zero-growth variants complete at search one. At 100
distractors:

| Family | Concentrated, repeat / rotate | Distributed, repeat / rotate |
|---|---|---|
| Service effect | 1 / 1 | 1 / 1 |
| Collection membership | 3 / 2 | 4 / 2 |
| Independent outcomes | 1 / 1 | 1 / 1 |
| Same-resource read/action | 1 / 1 | 1 / 1 |

Repeated collection membership has one no-gain call in the concentrated profile
and two in the distributed profile. Prefix packet bytes are 46,999 and 61,117
respectively. Maximum individual packet: **15,968 bytes**. Insertion reversal
preserves exact semantic trajectories. Authorization, full target contracts,
uncertainty and native teaching checks pass. Provider calls: **zero**.

This is a cost/boundary gradient and a set of regression guards. It does **not**
reproduce v4's severe starvation. Do not tighten the gate after seeing these results
or replace the original failing cases with this greener cohort. Four structural
cases are not 32 independent tasks; zero-growth profile duplication is deliberate.
Different field shapes and independent domains remain future coverage.

## Continuation and prior-suite verification

`eval-reports/resource-breadth-v5-service-proof/`: **7/7** PostgreSQL ownership and
continuation proofs pass. Exact replay does no work, new repeated-text provenance
and changed intent advance once, reconnect preserves state, and exposure changes
search identity. The test injects fixture work at the service seam; it does not
claim route_turn publication or live classifier coverage. The owned disposable
container was removed and its absence independently checked.

`eval-reports/trajectory-v4-harness-parity-20261030/`: all 18 original trajectories
and the forced-rejection diagnostic have **exact semantic parity** with
`discovery-trajectory-v4-baseline-20261030-r2`, excluding timing. Packets, ranks,
deferrals, receipts, outcomes and teaching measures match. The same six quality
failures remain. All three successful run artifact hash manifests were verified.

The first v5 invocation was a compile-only harness failure; see ERRATA.md. Its
evidence is retained. Cases and labels were not changed after freezing.
