# Continuation regression contract (frozen before implementation)

These are consumed, domain-neutral development regressions, not holdouts.
The native planner receives explicit hybrid recall; no labels enter its request.

| Case | Given | Required observation |
| --- | --- | --- |
| changed-need | An exposed producer with 8 or 64 distinct consumers; current recall leads with an independent operation | The independent operation reaches judgment in the first packet |
| connected-control | The same graph, current recall leads with a consumer | That consumer reaches judgment; its exposed producer remains available |
| rejection-recovery | A candidate rejected for one self-contained request is needed by another | Reconsideration must be possible without resetting ownership or spent transport budgets |
| same-request-control | Repeated request, exposure and generation | Reuse completion; do not replay provider work |

First factor: remove unconditional consumer promotion ahead of hybrid recall.
Keep visit ordering, connected closure, authorization, constraints, packet format,
16,000-byte cap, two initial roots and all transport budgets fixed. Test both
consumer counts. Existing connected and progress tests are regression controls.
Stop after this factor for review; do not silently bundle a visit-state redesign.

Fixture development note: the initial short independent contract fitted after
the consumers, so that control passed unchanged. Before any production change,
add a 160-repeat authored-contract variant alongside the short control to test
actual packet pressure. This refinement is exploratory, not a confirmatory win.

Arm A (remove consumer promotion) passed the new case but failed the existing
retained-producer continuation control. Arm B reserves first priority for the
leading unexposed recalled root, then promotes consumers before other roots
within the same visit round. Both use unchanged fixtures and budgets.

The latter two cases require service-level durable-state coverage. They are not
proved by planner tests. Execution iteration with an external stop set and
unobserved union fields are separate native execution contracts, not discovery
quality labels. No AppWorld or provider run is authorized by this registration.
