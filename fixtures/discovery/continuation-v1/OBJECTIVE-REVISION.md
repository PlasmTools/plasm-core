# Objective revision v1 — preregistered scheduling experiment

Reuse capacity-v1's exact abstract catalogues, widths, target labels, sizes,
positions, recall order, six-call horizon and 16,000-byte limit. Add only the
explicit objective-transition signal. Every current request remains complete;
the signal controls visit-count scope, not relevance, exposure or permissions.

Signals are Continue and Revise. Revise starts a fresh visit map; Continue retains
the objective's map. Native production ordering and allocation consume that map.
No semantic comparison, provider call, target-specific rule, new ranking signal,
or source contract truncation is introduced.

Freeze five caller arms:

1. Correct: Revise on call one only for the changed-constraint case; otherwise Continue.
2. Repeated false revision: Revise every call, including paraphrases.
3. Missed revision: always Continue, including genuine amendment.
4. One spurious revision: correct signals, plus Revise on call two.
5. Delayed revision: genuine amendment is marked Revise on call two instead of one.

The case label supplies the synthetic caller's signal; it is deliberately an
oracle in arm 1. This tests the mechanism conditional on caller behavior, not
whether an agent understands the contract. Never call arm 1 a realistic accuracy
measurement. Misclassification controls retain all failures.

Desired gates: target judgment within six calls; genuine amendment within two;
no cost-direction bias. Report delay relative to correct signals and the earlier
exact-text baseline. Keep baseline experiments unchanged in scope. Passing the
correct arm is necessary but insufficient for production adoption: caller
reliability, multi-process state and budget ownership remain untested.
