# Constant IDF: rejected

The single-factor ablation completed all 114 paired variants, including 16 fresh action-discrimination controls. Native matcher and reverse-insertion checks passed. Both experimental production files were restored byte-for-byte. No provider calls, effects, dependencies or AppWorld evaluation occurred.

| Cohort | Baseline four-search passes | Constant-IDF passes | Prefix requests before → after |
|---|---:|---:|---:|
| Starvation trajectories | 12/18 | 10/18 | 118 → 155 |
| Resource breadth | 32/32 | 28/32 | 39 → 71 |
| Ordinary controls | 8/8 | 8/8 | 8 → 8 |
| Catalogue partition stress | 25/40 | 20/40 | 234 → 292 |
| Fresh action discrimination | 16/16 | 8/16 | 16 → 80 |

No old starvation gate recovers. Nineteen baseline passes are lost, 25 variants exceed the allowed one-extra-search delay, and total prefix requests increase from 415 to 606. All eight ordinary controls remain one search; that narrow success does not offset the broader regressions.

Every fresh control with 100 distractors worsens from one search to nine, under both request schedules. This includes the ordinary-action control as well as the three rare-action controls. For repeated demagnetization, baseline places the requested operation first with score 7.356. Constant IDF puts it at rank 101, score 3.483, behind inspection records at 8.233. Broad overlap with transport, packaging, identifier and readiness outweighs the distinguishing action when rarity no longer helps. Zero-distractor controls remain one search.

The earlier diagnosis was real but incomplete: corpus rarity can amplify incidental context under growth, yet it also supplies useful action discrimination. Removing it does not make lexical retrieval understand requested actions, conditions or excluded outcomes. It can instead make broad contextual overlap more dominant.

Keep IDF and the production scheduler unchanged. Do not tune an IDF floor, field weight or action-word exception on these results. Catalogue quotas and constant rarity weighting are now both rejected implementations, with their evidence retained.

The next design review should address the information retrieval receives: how the current request and authored effect contracts express contribution, constraints, exclusions and independent outcomes. Separate that semantic representation question from deterministic path construction and budget admission. Any representation candidate needs fresh paired controls where the same words reverse roles between requested action and context, and must retain full contracts for judgment. Do not assume deterministic parsing can resolve arbitrary natural-language intent, and do not silently introduce a second model pass. The current evidence selects a problem boundary; it does not yet validate a replacement.

The 98 established baselines were reused from the exact-parity attribution run; the 16 fresh baselines were measured here. Instrumented runtime is not a speed comparison. These are controlled development cases with fixture judgment, not a holdout, provider-quality or full-agent result.
