"""Freeze/check fixtures and run the native, network-free discovery benchmark."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
from datetime import datetime, timezone

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
NATIVE = ROOT / 'crates/plasm-agent-core/src/discovery_matcher/online/fixed_suite.rs'

def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

def fixture_paths():
    return [HERE / name for name in ['cases.json', 'generate.py', 'PROTOCOL.md']] + list(
        sorted((ROOT / 'fixtures/schemas/discovery_fixed_v1').glob('*.yaml')))

def verify():
    lock = json.loads((HERE / 'LOCK.json').read_text())
    for name, expected in lock.items():
        assert sha(ROOT / name) == expected, f'Frozen fixture changed: {name}'
    cases = json.loads((HERE / 'cases.json').read_text())
    assert len(cases) == len({c['id'] for c in cases}) == 48
    for case in cases:
        assert set(case['labels']) == set(case['descriptions'])
        assert set(case['labels'].values()) <= {'relevant','uncertain','unrelated'}
        assert all(set(path) <= set(case['labels']) for path in case['expected_paths'])
        assert all(case['labels'][name] != 'unrelated' for path in case['expected_paths'] for name in path)
    return lock

def summarize(path):
    data = json.loads(path.read_text())
    counts = {}
    complete = {str(step): {'complete':0,'total':0} for step in [1,2,4]}
    bytes_used = requests = 0
    faults = []
    families = {}
    ranks = {'relevant': [], 'uncertain': []}
    for case in data['cases']:
        if 'error' in case:
            faults.append({'case':case['case'],'error':case['error']})
            continue
        family = families.setdefault(case['family'], {'cases':0,'paths_complete_at_4':0,'paths_total':0})
        family['cases'] += 1
        for turn in case['turns']:
            receipt = turn['receipt']
            assert receipt['input_bytes'] <= 32000 and receipt['requests'] <= 1
            bytes_used += receipt['input_bytes']; requests += receipt['requests']
            if str(turn['search']) in complete:
                bucket = complete[str(turn['search'])]
                bucket['total'] += len(turn['paths'])
                bucket['complete'] += sum(p['complete'] for p in turn['paths'])
            if turn['search'] == 4:
                family['paths_complete_at_4'] += sum(p['complete'] for p in turn['paths'])
                family['paths_total'] += len(turn['paths'])
                for item in turn['status']:
                    if item['expected_contribution'] == 'unrelated':
                        continue
                    key = item['stage']; counts[key] = counts.get(key,0)+1
            if turn['search'] == 1:
                for item in turn['status']:
                    label = item['expected_contribution']
                    if label in ranks and item['stage'] != 'unauthorized':
                        ranks[label].append(item['rank'])
    root_metrics = {label:{'targets':len(values),
                          'recall':{str(k):sum(v is not None and v<=k for v in values)/len(values)
                                    if values else None for k in [1,4,8,16]},
                          'mrr':sum(1/v for v in values if v is not None)/len(values) if values else None}
                    for label,values in ranks.items()}
    return {'cases':len(data['cases']), 'native_faults':faults,'path_coverage':complete,
            'final_useful_or_uncertain_stages':counts,'input_bytes':bytes_used,
            'fixture_requests':requests,'provider_calls':0,'provider_usage':None,
            'first_search_root_metrics':root_metrics,'families':families,
            'quality_is_not_harness_pass':True}

def main():
    command = sys.argv[1] if len(sys.argv)>1 else 'verify'
    if command == 'freeze':
        assert not (HERE/'LOCK.json').exists(), 'Do not overwrite the fixture lock'
        (HERE/'LOCK.json').write_text(json.dumps({str(p.relative_to(ROOT)):sha(p) for p in fixture_paths()},indent=2)+'\n')
        verify(); print('48 fixture definitions and labels frozen; no native scoring run.'); return
    verify()
    if command == 'verify':
        print('Frozen fixture hashes and label schema verified.'); return
    if command == 'summary':
        print(json.dumps(summarize(Path(sys.argv[2])),indent=2)); return
    assert command == 'run'
    destination = Path(sys.argv[2]).resolve() if len(sys.argv)>2 else ROOT/'target'/('discovery-fixed-v1-'+datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%SZ'))
    destination.mkdir(parents=True,exist_ok=False)
    sources = [NATIVE, HERE/'run.py'] + list((ROOT/'crates/plasm-agent-core/src/discovery_matcher').rglob('*.rs'))
    sources += [ROOT/'crates/plasm-agent-core/src/discovery_matcher.rs',ROOT/'crates/plasm-core/src/domain_lexicon.rs']
    manifest = {'fixture_lock':json.loads((HERE/'LOCK.json').read_text()),
                'sources':{str(p.relative_to(ROOT)):sha(p) for p in sources},
                'head':subprocess.check_output(['git','rev-parse','HEAD'],cwd=ROOT,text=True).strip(),
                'judgment':'fixture labels; no model accuracy claim'}
    (destination/'MANIFEST.json').write_text(json.dumps(manifest,indent=2)+'\n')
    report = destination/'RESULTS.json'
    env = dict(os.environ,PLASM_FIXED_DISCOVERY_REPORT=str(report))
    with (destination/'TEST.log').open('w') as log:
        result = subprocess.run(['cargo','test','-p','plasm-agent-core','--lib','fixed_native_discovery_suite_v1','--','--ignored','--nocapture'],cwd=ROOT,env=env,stdout=log,stderr=subprocess.STDOUT)
    verify()
    assert all(sha(ROOT/name)==value for name,value in manifest['sources'].items()), 'Source changed during run'
    if result.returncode:
        raise SystemExit(f'Native harness failed; preserve {destination}/TEST.log')
    summary = summarize(report)
    (destination/'SUMMARY.json').write_text(json.dumps(summary,indent=2)+'\n')
    (destination/'SHA256.json').write_text(json.dumps({p.name:sha(p) for p in destination.iterdir() if p.is_file()},indent=2)+'\n')
    print(json.dumps({'directory':str(destination),**summary},indent=2))
    if summary['native_faults']:
        raise SystemExit('Native faults recorded; suite is incomplete')

if __name__ == '__main__':
    main()
