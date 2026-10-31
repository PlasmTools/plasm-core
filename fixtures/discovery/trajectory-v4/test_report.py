"""The report must fail late/missing paths, not mistake harness success for quality."""
import unittest
from run import summarize


def case(first):
    return dict(id='abstract', distractors=100, rotating=True, timing={}, turns=[
        dict(search=i, outcomes=[first is not None and i >= first, True], newly_exposed=['x'] if i == first else [],
             receipt=dict(requests=1, input_bytes=100), packets=[dict(bytes=100)], declaration_bytes=200,
             status=[], relation_visible=False, repeated_rejections=0, journal_targets=1)
        for i in range(1, 25)])


class ReportTests(unittest.TestCase):
    def test_late_and_missing_are_failures(self):
        summary = summarize(dict(cases=[case(4), case(5), case(None)]))
        self.assertEqual(len(summary['quality_failures']), 2)
        self.assertEqual([r['first_complete'] for r in summary['cases']], [4, 5, None])
        self.assertEqual([r['missing_outcome_searches'] for r in summary['cases']], [3, 4, 24])

    def test_cost_stops_at_completion_without_hiding_tail(self):
        row = summarize(dict(cases=[case(4)]))['cases'][0]
        self.assertEqual(row['requests_to_completion_or_horizon'], 4)
        self.assertEqual(row['post_completion_requests'], 20)
        self.assertEqual(row['no_gain_before_completion'], 3)
        self.assertEqual(row['bytes_to_completion_or_horizon'], 400)


if __name__ == '__main__':
    unittest.main()
