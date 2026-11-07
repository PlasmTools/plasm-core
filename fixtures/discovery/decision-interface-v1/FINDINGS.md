# Decision-interface v1: separate classification from capability loss

The single authorized pass completed all 48 frozen packets (56 questions, of
which eight are auxiliary). Forty-two primary judgments matched their admissible
labels; 18 of 24 pairs matched. All 135 active native matcher tests passed before
provider access. No production algorithm or prompt changed.

## What happened

| Mechanism | Frozen judgments matched | Operational consequence |
|---|---:|---|
| Requested action versus contextual observation | 7/8 | One explicitly unnecessary inspection was retained as relevant |
| Effect direction and stage | 7/8 | Closing a gate was retained for a request to open it |
| Contribution versus current readiness | 8/8 | Missing current inputs did not suppress useful operations |
| Typed composition witness present versus absent | 5/8 | Three documented suppliers stayed uncertain despite the witness |
| Explicit constraints and conditional branches | 7/8 | One conditional polishing outcome stayed uncertain |
| Question-local isolation from an unrelated peer | 8/8 | Primary classifications stayed invariant |

The distinction between exact labels and operational retention is decisive:
all **26 known-useful targets were retained**. Four were uncertain rather than
relevant, which the current monotonic policy already retains. Of 16 explicitly
unrelated targets, two were retained. Both opaque targets remained uncertain.
All four deliberately incomplete-witness packets received an admissible answer;
their globally useful supplier was not mislabeled as a model false rejection.

The two false-positive answers had reported confidence 0.38 and 0.18. These are
descriptive observations, not calibrated probabilities or evidence for a threshold.
Adding a confidence gate after seeing these examples would be post-hoc tuning.

## What this does and does not establish

The current contribution-only rubric handles these simple readiness and scope
controls. This pass does not reproduce useful-operation rejection. It therefore
does not establish that the decision model caused the earlier discovery-call
explosion, or that discovery is now efficient. Ranking, packet admission, repeated
frontier progress, exposure and the agent loop were deliberately outside this test.

The packets contain only one or two questions and are 1,834–3,195 UTF-8 bytes,
well below the unchanged 16,000-byte ceiling. Evidence density, branching depth,
near-limit packets, real wording variety and repeated sampling remain untested.
Several negative controls explicitly state that they do not achieve the outcome;
these are diagnostic controls, not a representative production distribution.

## Design implication and next bounded experiment

Keep structural proofs deterministic: reference resolution, authorization, type
compatibility, witness direction, required-input coverage, packet bytes and
monotonic exposure. Keep semantic contribution and natural-language prohibitions
as semantic judgments. A type-compatible edge alone does not prove that an effect
is wanted, that an alternative supplier is necessary, or that execution is allowed.

The most specific interface weakness here is that three of four explicit typed
witnesses did not resolve supplier uncertainty. A suitable next one-factor test is
to render the *same verified witness* as an explicit source-field → consumer-input
connection beside the target, while preserving descriptions, rubric, target scope,
model, budgets and labels. This tests the burden of reconstructing distributed
references; it must not add inferred purpose or promote arbitrary graph neighbors.
Freeze fresh matched cross-domain controls before that comparison, retain opaque
and forbidden-effect controls, and report retention as well as exact labels.

Do not combine this with a new allocator or a confidence threshold. If rendering
does not help, a later separate experiment may judge downstream semantic anchors
and propagate only checked prerequisite support deterministically, without claiming
that exposure authorizes execution. That is a design hypothesis, not a result here.

The literature and responsibility analysis remain in
`docs/research/discovery-decision-responsibilities-20261030.md` in the parent repo.
This diagnostic is an empirical boundary check of that analysis, not an adoption
trial or an AppWorld score.

## Cost and integrity

48 logical requests, 48 transport attempts, all HTTP 200; no retries, unrecovered
faults, unpaired attempt starts or executed effects. Provider receipts report
27,391 input tokens, zero output tokens and total cost 0.0027391. Cache usage is
unavailable. Whole-judgment latency: median 638 ms, p90 758 ms, p95 871 ms, maximum
3,430 ms; sequential judgment time totals 31,711 ms, excluding build/preflight.
These are current measurements, not an improvement claim against a paired baseline.

Total serialized request-body bytes were 108,660. Dispatch lifecycle records do
not prove transmitted bytes. Production retry limits remained three attempts,
30 seconds total and eight seconds per attempt. No retry was exercised here.

The original preflight rejected an unused entity. Its inputs and failure were
preserved; revision 2 removed unused Token declarations before any model call.
Requests and labels are byte-identical to the original freeze. A local credential
lookup initially failed before launch because the environment file uses `export`;
the unchanged runner then received the configured key through its supported
environment input. The copied private credential was removed after completion.

Evidence: `eval-reports/decision-interface-v1-20261030-r2/` in plasm-oss contains
PACKETS.json, CONTRAST-CHECKS.json, TRACE.jsonl, AUDIT.json, REPORT.md, REPORT.html,
source/packet hash manifests, gate logs and cleanup confirmation. The frozen
fixtures, labels and preparation errata are alongside this document.
