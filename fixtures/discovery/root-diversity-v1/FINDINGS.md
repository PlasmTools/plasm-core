# Entity-diverse roots: rejected development candidate

The frozen protocol rejected this candidate. Production `root_order` and `SEARCH_POLICY` were restored byte-for-byte to the baseline source hashes. No dependency, provider, AppWorld, or execution changes were made. The retained candidate sketch is in `candidate.patch`; it is not active code.

All six previously failing four-search trajectory gates recovered, but the ordinary zero-distractor independent-outcome case took two searches instead of one. That violates the preregistered ordinary-case criterion even though its four-search gate remains green. Do not use 18/18 as an adoption claim.

| Case | Distractors | Schedule | Complete before → after | Prefix request delta | Prefix byte delta |
|---|---:|---|---|---:|---:|
| Collection | 0 | repeat | 1 → 1 | 0 | 0 |
| Collection | 0 | rotate | 1 → 1 | 0 | 0 |
| Collection | 100 | repeat | 10 → 1 | -9 | -142021 |
| Collection | 100 | rotate | 2 → 1 | -1 | -14349 |
| Collection | 1000 | repeat | incomplete at 24 → 1 | -23 | -365662 |
| Collection | 1000 | rotate | 2 → 1 | -1 | -14349 |
| Independent | 0 | repeat | 1 → 2 | +1 | +9154 |
| Independent | 0 | rotate | 1 → 2 | +1 | +9125 |
| Independent | 100 | repeat | 11 → 2 | -9 | -132939 |
| Independent | 100 | rotate | 11 → 2 | -9 | -138712 |
| Independent | 1000 | repeat | incomplete at 24 → 2 | -22 | -323026 |
| Independent | 1000 | rotate | incomplete at 24 → 2 | -22 | -328572 |
| Service | 0 | repeat | 1 → 1 | 0 | +890 |
| Service | 0 | rotate | 1 → 1 | 0 | +890 |
| Service | 100 | repeat | 1 → 1 | 0 | +927 |
| Service | 100 | rotate | 1 → 1 | 0 | +927 |
| Service | 1000 | repeat | 1 → 1 | 0 | +952 |
| Service | 1000 | rotate | 1 → 1 | 0 | +952 |

All eight new ordinary counter-control variants stayed at one search and one request. Descriptive read, known-input action, and same-resource read/action had zero prefix byte change in both schedules. Independent preparation/publication used 890 extra bytes in each schedule. These are four structural cases with two request schedules, not eight independent domains.

## Mechanical explanation

Entity diversity repairs resource breadth, but entity ownership is not an outcome or path. In the zero-growth independent case, `fire` remains lexical rank 4 but falls to frontier rank 9 because other operations on its entity consume that entity's earlier positions. It is budget-deferred without judgment on search one. Preparation, publication, the opaque routine and sample read are retained. Search two retains `fire`. The original packet retained all five in search one. This is allocation displacement, not model rejection or loss of uncertainty.

## Evidence and limits

- Baseline trajectories: `eval-reports/discovery-trajectory-v4-baseline-20261030-r2/`.
- Candidate trajectories: `eval-reports/root-diversity-v1-trajectories/`.
- Counter-controls: `eval-reports/root-diversity-v1-baseline/` and `eval-reports/root-diversity-v1-counter-candidate/`.
- Each has raw results, source/fixture manifests and artifact hashes. Use trajectory-v4/compare.py for either paired comparison; input locks must match.
- Native integrity and reversed-insertion parity passed; fixed 16,000-byte packets and one logical fixture judgment per search.
- Trajectory test execution was 97.8 seconds before and 171.7 seconds after, excluding builds. Single-run timings are diagnostic, not an isolated performance estimate; the full 24-search horizon includes post-completion work.
- Summed forward-run selection time through completion (or the horizon when incomplete) fell from 19,940.4 ms to 4,210.8 ms. Summed selection time over all forced 24-search trajectories rose from 41,378.4 ms to 75,782.9 ms. These are different estimands: the latter forces searches after success. Neither includes provider time. The rejection rests on the ordinary-case search regression, not an unqualified claim that the candidate is slower end to end.
- No provider calls. Scripted requests and fixture labels cannot establish classifier quality, adaptive-agent stopping, execution success, or generalization.

The next design review must distinguish diversity of authored resources from completeness of useful paths. Keep this null result; do not silently replace its criterion or tune these labels to make the candidate pass.
