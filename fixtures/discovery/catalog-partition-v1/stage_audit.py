"""Post-hoc diagnostic of recorded stages; never changes the frozen scores."""
import hashlib
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
SOURCE = ROOT / 'eval-reports/catalog-partition-v1-20261030'
OUT = ROOT / 'eval-reports/discovery-admission-design-20261030'

def main():
    OUT.mkdir(exist_ok=False)
    result = {'scope': 'Post-hoc recorded-trace diagnosis; target labels are not independent outcomes or ranking instructions.', 'arms': {}}
    for arm in ['baseline', 'candidate']:
        path = SOURCE / arm / 'RESULTS.json'
        raw = path.read_bytes()
        data = json.loads(raw)
        cases = []
        for case in data['cases']:
            first = next((t['search'] for t in case['turns'] if all(t['outcomes'])), None)
            prefix = case['turns'][:first] if first else case['turns']
            cases.append({
                'id': case['id'], 'rotating': case['rotating'], 'first_complete': first,
                'first_search_targets': case['turns'][0]['status'],
                'repeated_rejections_before_completion': sum(t['repeated_rejections'] for t in prefix),
                'searches_without_new_exposure_before_completion': sum(not t['newly_exposed'] for t in prefix),
                'first_search_outcomes': case['turns'][0]['outcomes'],
                'first_search_relation_visible': case['turns'][0]['relation_visible'],
            })
        result['arms'][arm] = {
            'source_sha256': hashlib.sha256(raw).hexdigest(), 'cases': cases,
            'prefix_repeated_rejections': sum(c['repeated_rejections_before_completion'] for c in cases),
        }
    (OUT / 'STAGE-AUDIT.json').write_text(json.dumps(result, indent=2) + '\n')
    print('Recorded-stage audit saved; no discovery or provider work executed.')

if __name__ == '__main__':
    main()
