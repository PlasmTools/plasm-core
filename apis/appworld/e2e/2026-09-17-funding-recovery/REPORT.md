# Funding recovery contract verification

The discovery assessor now receives operational_goal separately from the original workflow. The host assigns f0, requires its assessment, binds it into request provenance and cache input, and includes its capabilities in selection/recovery. An unresolved f0 prevents Ready. An already-exposed capability can support f0 without a redundant addition. Original user constraints and interpretation decisions remain intact; agent-reported failures do not authorize broader effects.

Catalog changes: Venmo transfer and request-approval payment_card_id descriptions explain linked-card funding versus the default Venmo balance. Supervisor payment_card_query declares cvv_number in provides, matching its OpenAPI response.

## Verification

- `cargo test -p plasm-agent-core --lib workflow_intent --quiet`: 33 passed.
- `cargo test -p plasm-agent-core --lib discovery_service::tests --quiet`: 7 passed, including operational recovery and provenance tampering.
- `cargo check -p plasm --example selector_contract --quiet`: passed (existing unused-code warnings). The offline command now requires a focus string; use an empty string for workflow-only assessment.
- Abstract discovery recovery regression exercises a consumer whose credential expired and a renewal producer; verifies request focus, mandatory f0, selection of new/already-exposed producers, unresolved status, preserved workflow, changed request body, and rejection of altered goal provenance. No AppWorld-specific fixture.
- Existing Hermit `plasm-cgs validate --spec ... --require-complete ...`: Venmo 66 pass / 1 fail, Supervisor 6 pass / 1 fail. Funding/card checks pass. Residual failures remain generated identity `test-1` for Venmo Friend and Supervisor AccountPassword derived Gets; not fixed or counted as passes. Full logs adjacent.
- No dependencies changed. No AppWorld run, live payment, or provider-model evaluation. Engine binary/container not rebuilt. Model selection quality remains unmeasured after the change.
