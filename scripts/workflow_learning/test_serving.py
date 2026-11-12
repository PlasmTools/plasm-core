"""Offline serving contract model; does not emulate Rust compilation or storage."""

from copy import deepcopy
from dataclasses import dataclass, field
import unittest


class FetchError(Exception):
    pass


@dataclass
class State:
    scope: tuple = ("tenant-a", "project-a")
    revision: int = 0
    authorization_revision: int = 0
    example_revision: int = 1
    allowed: set = field(default_factory=lambda: {"ledger.read", "ledger.write"})
    symbols: dict = field(default_factory=lambda: {"ledger.read": "m1"})
    ledger: dict = field(default_factory=lambda: {"ledger.read": "m1"})
    receipts: dict = field(default_factory=dict)
    quarantine: set = field(default_factory=set)


def stage(state, scope, dependencies, fingerprint, checker):
    if scope != state.scope or not dependencies <= state.allowed:
        raise FetchError("inapplicable")
    if (state.example_revision, fingerprint) in state.quarantine:
        raise FetchError("quarantined")
    symbols = dict(state.symbols)
    for dependency in sorted(dependencies):
        if dependency not in symbols:
            symbols[dependency] = f"m{len(symbols) + 1}"
    result = checker(symbols)
    if result == "type_error":
        state.quarantine.add((state.example_revision, fingerprint))
        raise FetchError("type_error")
    if result != "valid":
        raise FetchError("checker_unavailable")
    return {
        "scope": scope,
        "revision": state.revision,
        "authorization_revision": state.authorization_revision,
        "example_revision": state.example_revision,
        "fingerprint": fingerprint,
        "dependencies": dependencies,
        "symbols": symbols,
    }


def commit(state, prepared, receipt_id, fail_at=None):
    if (prepared["scope"] != state.scope
            or not prepared["dependencies"] <= state.allowed):
        raise FetchError("inapplicable")
    for key in ("revision", "authorization_revision", "example_revision"):
        if prepared[key] != getattr(state, key):
            raise FetchError("conflict")
    if (state.example_revision, prepared["fingerprint"]) in state.quarantine:
        raise FetchError("quarantined")
    # Model one transaction: intermediate writes affect only a private copy.
    transaction = deepcopy(state)
    transaction.symbols = dict(prepared["symbols"])
    if fail_at == "session":
        raise FetchError("persistence")
    transaction.ledger = dict(prepared["symbols"])
    if fail_at == "ledger":
        raise FetchError("persistence")
    transaction.revision += 1
    transaction.receipts[receipt_id] = {
        "symbols": dict(prepared["symbols"]),
        "revision": transaction.revision,
        "example_revision": prepared["example_revision"],
    }
    if fail_at == "receipt":
        raise FetchError("persistence")
    state.__dict__.update(transaction.__dict__)
    return deepcopy(state.receipts[receipt_id])


def receipt(state, scope, receipt_id):
    value = state.receipts[receipt_id]
    if scope != state.scope or not set(value["symbols"]) <= state.allowed:
        raise FetchError("inapplicable")
    return deepcopy(value)


class ServingContract(unittest.TestCase):
    def prepare(self, state, checker=lambda _: "valid"):
        return stage(state, ("tenant-a", "project-a"), {"ledger.read", "ledger.write"}, "cg1-py1", checker)

    def test_compile_failure_does_not_publish_symbols(self):
        state = State()
        before = deepcopy(state)
        with self.assertRaisesRegex(FetchError, "type_error"):
            self.prepare(state, lambda _: "type_error")
        self.assertEqual(state.symbols, before.symbols)
        self.assertEqual(state.ledger, before.ledger)
        self.assertEqual(state.revision, before.revision)
        self.assertEqual(state.quarantine, {(1, "cg1-py1")})

    def test_checker_outage_does_not_quarantine(self):
        state = State()
        before = deepcopy(state)
        with self.assertRaisesRegex(FetchError, "checker_unavailable"):
            self.prepare(state, lambda _: "unavailable")
        self.assertEqual(state, before)

    def test_scope_denial_precedes_checker_or_disclosure(self):
        state = State()
        def forbidden(_):
            self.fail("checker must not see unauthorized dependencies")
        with self.assertRaisesRegex(FetchError, "inapplicable"):
            stage(state, ("tenant-b", "project-a"), {"ledger.read"}, "cg1-py1", forbidden)
        self.assertFalse(state.quarantine)

    def test_project_denial_precedes_checker_or_disclosure(self):
        state = State()
        with self.assertRaisesRegex(FetchError, "inapplicable"):
            stage(state, ("tenant-a", "project-b"), {"ledger.read"},
                  "cg1-py1", lambda _: self.fail("cross-project checker"))
        self.assertFalse(state.quarantine)

    def test_missing_prerequisite_rejects_complete_closure(self):
        state = State()
        with self.assertRaisesRegex(FetchError, "inapplicable"):
            stage(state, state.scope, {"ledger.write", "identity.acquire"},
                  "cg1-py1", lambda _: self.fail("unauthorized checker"))

    def test_every_persistence_failure_leaves_aggregate_unchanged(self):
        for failure in ("session", "ledger", "receipt"):
            with self.subTest(failure=failure):
                state = State()
                prepared = self.prepare(state)
                before = deepcopy(state)
                with self.assertRaisesRegex(FetchError, "persistence"):
                    commit(state, prepared, "fetch-1", fail_at=failure)
                self.assertEqual(state, before)

    def test_existing_symbols_stable_and_receipt_retrieval_idempotent(self):
        state = State()
        result = commit(state, self.prepare(state), "fetch-1")
        self.assertEqual(result["symbols"], {"ledger.read": "m1", "ledger.write": "m2"})
        committed = deepcopy(state)
        self.assertEqual(receipt(state, state.scope, "fetch-1"), result)
        self.assertEqual(state, committed)

    def test_concurrent_writer_invalidates_staged_allocation(self):
        state = State()
        first, second = self.prepare(state), self.prepare(state)
        commit(state, first, "fetch-1")
        committed = deepcopy(state)
        with self.assertRaisesRegex(FetchError, "conflict"):
            commit(state, second, "fetch-2")
        self.assertEqual(state, committed)

    def test_commit_rechecks_revocation(self):
        state = State()
        prepared = self.prepare(state)
        state.allowed.remove("ledger.write")
        before = deepcopy(state)
        with self.assertRaisesRegex(FetchError, "inapplicable"):
            commit(state, prepared, "fetch-1")
        self.assertEqual(state, before)

    def test_commit_rechecks_example_quarantine(self):
        state = State()
        prepared = self.prepare(state)
        state.quarantine.add((1, "cg1-py1"))
        before = deepcopy(state)
        with self.assertRaisesRegex(FetchError, "quarantined"):
            commit(state, prepared, "fetch-1")
        self.assertEqual(state, before)

    def test_old_fingerprint_quarantine_does_not_block_new_validation(self):
        state = State(quarantine={(1, "cg0-py1")})
        prepared = self.prepare(state)
        self.assertEqual(prepared["fingerprint"], "cg1-py1")


if __name__ == "__main__":
    unittest.main()
