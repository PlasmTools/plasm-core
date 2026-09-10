# τ³-Banking — Plasm CGS

Capability graph for Sierra **τ³-bench** `banking_knowledge` (`tau2-bench` v1.0.1).

**Scope:** 56 capabilities across 17 entities (v12). Auth: `scheme: none` (τ² shim at `http://127.0.0.1:18080`).

## Doctrine

Plasm **replaces** JSON discoverable-tool theatre. Catalog capabilities are semantic wires IntentOnly / auto-seed may teach from task intent + authored text. Exact Python tool names (`freeze_debit_card_3892`, …) live only in CML paths and [`unlock_registry.json`](../../../scripts/tau3_banking/unlock_registry.json) so the shared-DB shim can mutate the grader’s `TransactionalDB` (shim auto-unlocks under the hood).

**Not in the CGS:** `unlock_discoverable_agent_tool` / `call_discoverable_agent_tool` / `list_discoverable_agent_tools` / `LockedToolkit`. Dual-control **`give_discoverable_user_tool`** remains on `AgentToolkit`.

Entity layout (User / CC / BankAccount / DebitCard / …) is **domain modeling**, not a ranked unlock cage.

## Credit limit increase (task-compressed)

- Entity `CreditLimitRequest` — submit / list / deny / approve, with `nv_cli_denial_reason` select.
- Abstract `CreditLimitEligibility` + `views.credit_limit_eligibility` — one query composing prior CLI history, 6-month payments, disputes, and pending replacements.

τ²’s unified `decide_credit_limit_increase` tool is **not** modeled; approve and deny are separate capabilities with symmetric `provides` witnesses.

## Eval

```bash
cargo run -p plasm-eval --features baml -- coverage --schema apis/tau3_banking --cases apis/tau3_banking/eval/cases.yaml
```

Cases cover CLI workflow buckets and entity coverage for the teaching table. Live DB grading is owned by [`scripts/tau3_banking/compare_pilot.py`](../../../scripts/tau3_banking/compare_pilot.py).

## Compare pilot bundle

Distributable archive (CGS + metrics + report + full sim `results.json` when present):

```bash
python3 scripts/tau3_banking/bundle_compare_release.py --archive
# → scripts/tau3_banking/dist/tau3-banking-compare-20260824.tar.gz
```

## Harness

[`scripts/tau3_banking/`](../../../scripts/tau3_banking/) · [`docs/tau3-banking-plasm.md`](../../../docs/tau3-banking-plasm.md)

```bash
cargo run -p plasm-cli --bin plasm-cgs -- schema validate apis/tau3_banking
python3 plasm-oss/scripts/check_catalog_description_hygiene.py --catalog tau3_banking --fail-on error
```
