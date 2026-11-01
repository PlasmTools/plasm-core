"""Cross-package offline simulation; provider calls and Rust are not emulated."""

import tempfile
import unittest

from test_evidence_portfolio import Evidence, Example, Ledger
from test_teaching_index import Candidate, SemanticRef, canonical_shape, occurrence, retrieve, rewrite
from test_serving import FetchError, State, commit, stage


class JointContract(unittest.TestCase):
    def test_review_promote_recall_rebind_and_atomic_serve(self):
        source = "result = e1.m1()\n"
        entity = SemanticRef("entity", "ledger", "Account", "")
        method = SemanticRef("method", "ledger", "Account", "read")
        refs = [occurrence(source, "e1", entity), occurrence(source, "m1", method)]
        identity = canonical_shape(source, refs)
        with tempfile.TemporaryDirectory() as directory:
            ledger = Ledger(directory + "/learning.sqlite")
            try:
                review = ledger.review(Evidence(source=source))
                fence = ledger.claim(review, now=0)
                scope = ledger.scope()
                ledger.promote(review, fence, 1, scope, 0, Example(identity))
                self.assertEqual(len(ledger.rows("active")), 1)
                examples = [Candidate(identity, frozenset({"inspect"}), frozenset({"ledger"}), "v1")]
                self.assertEqual(retrieve({"inspect"}, {"ledger"}, examples)[0].name, identity)
                self.assertEqual(retrieve({"inspect"}, set(), examples), [])
                rebound = rewrite(source, refs, {entity: "e8", method: "m9"})
                self.assertEqual(rebound, "result = e8.m9()\n")
                state = State()
                prepared = stage(state, ("tenant-a", "project-a"), {"ledger.read", "ledger.write"}, "v1", lambda _: "valid")
                old_symbols = dict(state.symbols)
                with self.assertRaises(FetchError):
                    commit(state, prepared, identity, fail_at="receipt")
                self.assertEqual(state.symbols, old_symbols)
                commit(state, prepared, identity)
                self.assertEqual(state.symbols, state.ledger)
            finally:
                ledger.close()


if __name__ == "__main__":
    unittest.main()
