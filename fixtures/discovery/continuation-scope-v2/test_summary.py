import unittest
import itertools
from summarize import summarize, validate_matrix


class QualityControls(unittest.TestCase):
    def test_matrix_completeness(self):
        keys = ("oversized", "changed", "rotating", "shared_exploration")
        rows = [{**dict(zip(keys, values)), "turns": [{"turn": n} for n in range(1, 7)]}
                for values in itertools.product((False, True), repeat=4)]
        validate_matrix(rows)
        for bad in ([], rows[:-1], rows[:-1] + [rows[0]],
                    [{**rows[0], "turns": []}] + rows[1:]):
            with self.assertRaises(ValueError):
                validate_matrix(bad)

    def cell(self, hits, changed=False, oversized=False):
        return {"changed": changed, "oversized": oversized, "turns": [
            {"turn": i + 1, "target_relevant": hit, "unique": 1,
             "repeated": 0, "bytes": 100} for i, hit in enumerate(hits)]}

    def test_late_success_does_not_erase_failure_at_gate(self):
        self.assertEqual(summarize([self.cell([False] * 5 + [True])])["quality_failures"], 1)

    def test_old_success_does_not_satisfy_changed_intent(self):
        self.assertEqual(summarize([self.cell([True] * 3 + [False] * 3, changed=True)])["quality_failures"], 1)

    def test_oversized_non_admission_is_integrity_control(self):
        self.assertEqual(summarize([self.cell([False] * 6, oversized=True)])["quality_failures"], 0)

    def test_changed_intent_requires_prompt_reconsideration(self):
        self.assertEqual(summarize([self.cell([True, False, False, True, False, False], changed=True)])["quality_failures"], 0)


if __name__ == "__main__":
    unittest.main()
