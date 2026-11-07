# Result and design correction

Do not adopt this renderer. The pre-registered gate failed. All 48 requests
completed, with no retry or transport fault. Baseline matched 13/24 frozen
admissible labels; inline matched 14/24. Both classified the four supporting
suppliers as relevant. The sole changed answer was a different-outcome supplier:
relevant became uncertain. Both labels retain the capability. **Capability
retention was identical across all 24 paired cases.**

Both arms classified all four suppliers connected to explicitly prohibited effects
as relevant. This conflicts with the diagnostic's local contribution labels, but
does not establish an execution violation or an operational discovery regression.
The target is a supplier, not execution of the downstream effect. Exposure neither
authorizes nor performs that effect. Incidental exposure matters operationally when
it consumes scarce budget, burdens the caller, or delays useful capabilities; this
small fixed-target experiment did not measure those consequences.

The user's correction is decisive: we were narrowing the design gradient to a
classifier-quality problem again. Discovery's purpose is to make sufficient useful
capabilities available promptly and cheaply for task completion. It is monotonic
and additive; it is not a perfect semantic filter, an execution plan, or an action
authorization system. Previously exposed capabilities remain available. Improving
a relevant-versus-uncertain distinction has no retention benefit under the current
policy and is not a product improvement by itself.

## Evidence

- 135 active matcher tests passed; native export validated all 48 packets.
- Candidate added only a mechanically derived local connection field; removing
  it reproduced the baseline body exactly. Eight control pairs were byte-identical.
- 24 baseline calls used 14,632 input tokens; 24 inline calls used 14,936 (+304).
- Provider-reported costs: baseline 0.0014632, inline 0.0014936; zero reported output
  tokens. Cache usage was not reported.
- Median whole-judgment latency: 530 ms baseline, 559 ms inline. Small, single-pass
  samples do not establish a latency change. Both arms used 24 transport attempts.
- No useful retention loss, no retry, no unrecovered fault, no unpaired attempt,
  and no observed attempt above eight seconds. The private key copy was removed.
- All 24 pairs retained exactly the same capabilities. No production cutover,
  effects, embeddings, or AppWorld rerun occurred.

## Restore the operational design gradient

Use the existing frozen multi-search trajectories as the primary comparison, with
ordinary cases alongside distractor growth, alternative suppliers, cross-catalogue
composition and opaque evidence. Do not replace their labels after seeing results.
The semantic diagnostics remain secondary attribution tools.

Primary measurements should be useful complete-path availability, discovery calls
and cumulative tokens/bytes to availability, whole discovery latency, declaration
growth, repeated judgment, and progress after rejection or deferral. An extra
capability is not automatically a failure: measure its actual cost or interference.
Distinguish maximum available capability coverage from a minimal execution plan.

Review the whole responsibility chain before selecting the next single-factor
intervention:

| Component | Operational question |
|---|---|
| Caller/request contract | Does each self-contained request express the needed capability without repeated blind reformulation? |
| Retrieval/ranking | Can useful outcomes and alternatives enter the candidate set at catalogue scale? |
| Allocation | Do fixed limits permit sufficient connected paths and independent outcomes? |
| Judgment | Does this gate avoid harmful exclusions or save enough exposure cost to justify its calls? |
| Frontier/session state | Does each request make additive progress without replaying spent work or starving alternatives? |
| Exposure/teaching | Can the caller locate and compose the available capabilities without excessive declaration growth? |
| Execution | Are concrete values, applicability, effects and permissions checked at their proper boundary? |

A deterministic type-compatible connection is structural evidence, not proof of
desired purpose or execution permission. Equally, the model should not repeatedly
prove structural facts the host already knows. Evaluate whether moving a boundary
actually improves the operational metrics before investing in finer classification.

Any next experiment should be frozen before running, change one mechanism, preserve
budgets and negative/opaque controls, and report all operational tradeoffs. Do not
infer a full-agent success rate from fixed-target semantic accuracy. No further
model experiment is launched by this report.
