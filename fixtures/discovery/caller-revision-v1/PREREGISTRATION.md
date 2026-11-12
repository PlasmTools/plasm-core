# Caller revision v1

Hypothesis: the existing Luna caller can apply an explicit objective-transition
contract without recreating packet starvation. One fixed prompt and model arm.
All 24 fresh neutral transitions are frozen exploratory development data, not a
sealed holdout. They have not been used for prompt tuning. No confirmatory or
generalization headline will be drawn from this smoke.

Model: openai/gpt-6-luna, reasoning high, requested temperature 0, maximum output
16,384 tokens, 300,000 ms per logical request. One logical request per case,
sequential, no semantic retries. Use the existing AppWorld caller resolver and
its explicit-429 recovery (at most eight attempts, bounded 120-second throttle
window); disable outer SDK retries. Abort on first unrecovered provider/harness
fault. Preserve invalid model outputs as failures; never repair or retry them.
No tools, task effects, new embeddings, production classifier or dependency edits.

The prompt contains only the domain-neutral contract. Send previous request/
events/current request; never send labels, replay mappings or native results.
Each input must fit 16,000 UTF-8 bytes. No seeds are set; provider determinism is
not assumed. Cases and labels must be hashed before any model call.

Strict judge: valid JSON with exactly signal and reason, valid signal enum and
nonempty string reason. Any other response fails. Count false revisions and
missed revisions separately. Desired gate: 24/24 valid correct signals and no
replay opportunity loss; tiny-sample success is insufficient for adoption.
Stop after the single frozen arm; no prompt changes after viewing outcomes.

Replay uses the already verified native objective-revision SUMMARY.json as an
exact lookup for the registered one-transition signal patterns. Stable traces
map to a signal on call two: continue -> ExplicitCorrect; revise ->
OneSpuriousRevision. Amendment traces map to call one: revise -> ExplicitCorrect;
continue -> MissedRevision. All subsequent calls are assumed Continue. This is
conditional replay, NOT a closed-loop agent test or a new native execution.

For stable IDs, suffixes 01/02/03/04 map to ExpensiveFirst/ExpensiveTarget/Equal/
Connected, respectively; use both distractor sizes and both target positions.
Amendment IDs map to Changed at those same four size/position combinations.
Invalid outputs have no replay and count as unresolved, not implicit Continue.
Report all projected observations and their dependence; they are not independent
trials. Freeze source and lookup hashes along with inputs. Report usage once from
SDK responses, attempt counts from wire events, and missing cost/cache unavailable.

Limit: traces are authored and compact, rather than raw agent tool histories.
This is an instructed contract smoke, not proof of realistic caller reliability.
Any follow-on naturalistic trace test requires fresh frozen cases.
