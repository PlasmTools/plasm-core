"""Compare every frozen trajectory; never hide regressions behind a total."""
import argparse
import json
from pathlib import Path


def load(directory):
    return (json.loads((directory / 'MANIFEST.json').read_text()),
            json.loads((directory / 'SUMMARY.json').read_text()))


def compare(before, after):
    def indexed(summary):
        return {(r['id'], r['distractors'], r['rotating']): r for r in summary['cases']}
    old, new = indexed(before), indexed(after)
    assert old.keys() == new.keys(), 'Cannot compare different case cohorts'
    rows = []
    for key in sorted(old):
        a, b = old[key], new[key]
        rows.append(dict(id=key[0], distractors=key[1], rotating=key[2],
            before_complete=a['first_complete'], after_complete=b['first_complete'],
            recovered=not a['gate_pass'] and b['gate_pass'],
            regressed=a['gate_pass'] and not b['gate_pass'],
            missing_outcome_search_delta=b['missing_outcome_searches']-a['missing_outcome_searches'],
            no_gain_delta=b['no_gain_before_completion']-a['no_gain_before_completion'],
            request_delta=b['requests_to_completion_or_horizon']-a['requests_to_completion_or_horizon'],
            byte_delta=b['bytes_to_completion_or_horizon']-a['bytes_to_completion_or_horizon']))
    return rows


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('baseline', type=Path)
    parser.add_argument('candidate', type=Path)
    args = parser.parse_args()
    am, a = load(args.baseline)
    bm, b = load(args.candidate)
    assert am['fixtures'] == bm['fixtures'], 'Frozen inputs differ; not a paired comparison'
    print(json.dumps(compare(a, b), indent=2))
