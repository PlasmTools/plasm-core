# Objective identity at the service persistence seam

Frozen before execution. Exploratory deterministic development proofs; no holdout score.

Hypothesis: an explicit objective epoch can preserve frontier visits across paraphrases
and observations while separating revisions, using existing durable completion and
ownership primitives without duplicate budget reservation.

Single factor: test-only frontier identity replaces exact current text with an epoch.
Search identity retains the production search hash and adds the epoch scope. Production
generation, permission, model and exposure key isolation remains covered by existing tests.
The experimental helper uses one fixed generation/model/authorization fixture.

Cases: continuation across three requests; exact replay from another store; same ancestry
in a fresh epoch; late old completion while a new epoch completes; concurrent duplicate.
Existing service concurrency tests cover durable reconstruction, interrupted owner,
cancelled waiter, checkpoint repair, and 24 distinct owners. Run the whole concurrency
module including its nonignored identity test and ignored PostgreSQL tests.

Judge: all assertions pass, no duplicate work closure for a completed request, one durable
reservation per novel request, and old completion cannot modify the new epoch frontier.
Stop on failed assertions; preserve logs. No repeated tuning or live model calls.

Fixed: production persistence, advisory locks, reservation implementation, all discovery
budgets, source dependencies. Abstract recorded receipt substitutes for provider judgment;
the existing hybrid-service proof independently exercises real retrieval and a fake provider.

Limits: not a public route_session/host protocol integration, not full transport-budget
accounting, not objective-epoch assignment under conflicting concurrent client amendments,
not proof of task quality. No production behavior promotion, effects, embeddings or AppWorld.
