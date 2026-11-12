# Role reversal disappears before judgment

All eight paired conditions produce identical initial lexical scores, rankings and admission traces despite reversing which action is requested and which is prohibited. The original, distinct current requests remain present in all 16 first judgment packets. This is a retrieval representation loss, not loss of the full request at the judgment boundary.

With no distractors, all eight variants complete in one search: both actions fit, and the fixture judgment can select the requested one. Identical ranking is therefore not itself proof of whole-system failure or unsafe execution. Exposure never authorizes an effect.

With 100 topical historical-record distractors:

| Action pair | Left requested | Right requested |
|---|---:|---:|
| Seal / inspect vessel | 2 searches | 1 search |
| Encrypt / archive document | 11 searches | 1 search |
| Demagnetize / weigh component | 11 searches | 11 searches |
| Desiccate / photograph specimen | 11 searches | 11 searches |

Six desired actions are budget-deferred in the first search. Five take 11 searches, failing the existing four-search availability gate. Their target lexical ranks are 101 or 102. Two right-hand targets do reach the first packet despite low lexical ranks, showing that rank alone is not an admission trace; the typed allocation machinery also matters.

The judgment in this experiment is an oracle fixture, not a model: it correctly distinguishes the opposite roles once the target is admitted. Thus these delays cannot be blamed on model rejection, temperature, instruction wording or missing negation in the judgment packet. They demonstrate a pre-judgment capacity problem for this constructed request family. They do not establish how often real callers produce these forms.

## What this rules out

Changing IDF or field weights cannot distinguish paired requests whose normalized query term sets are identical. Catalogue/entity quotas do not recover the missing role information either. Extra downstream judgment instructions cannot promote an absent candidate. Those are structural limits, not evidence that every semantic-retrieval design will work.

## Next design boundary

The next candidate must either preserve action/constraint roles before bounded admission, or provide broader candidate access without sacrificing the packet budget and complete contracts. Compare those interface choices before choosing a parser, embedding model or another scheduling heuristic. Keep the original request for judgment, preserve independent/conditional outcomes, and allow uncertainty when roles cannot be resolved. Do not silently solve natural-language intent with task-specific verbs, punctuation rules or a second unbudgeted model call.

Use these pairs as counterexamples alongside ordinary requests, rare-action controls, composition and catalogue-partition stress. Measure whether useful operations reach judgment promptly, not merely whether a derived query looks plausible. Any extraction component must be measured separately so extraction errors are not confused with retrieval or allocation errors. Caller progress and terminal completion still require their own checks.

## Execution evidence

The native paired test and reverse-insertion checks passed. Policy and budgets were unchanged; only test code and fixtures were added. No providers, effects, dependency changes or AppWorld run. The Bazel invocation took 547.7 seconds including compilation; native test execution took 42.0 seconds. These are operational timings, not a discovery speedup measurement.

This is a frozen constructed diagnostic with abstract contracts, not a holdout or a validated replacement design.
