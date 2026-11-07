"""One frozen scheduling candidate, always restored after measurement."""
import difflib
import hashlib
import json
import subprocess
from pathlib import Path

HERE=Path(__file__).resolve().parent
ROOT=HERE.parents[2]
OUT=ROOT/'eval-reports/catalog-breadth-v1-20261030'
FILES={'connected':ROOT/'crates/plasm-agent-core/src/discovery_matcher/online/connected.rs',
       'online':ROOT/'crates/plasm-agent-core/src/discovery_matcher/online.rs'}
COHORTS=['trajectory-v4','resource-breadth-v5','root-diversity-v1']


def sha(p):return hashlib.sha256(p.read_bytes()).hexdigest()


def verify(mapping):
    assert all(sha(ROOT/n)==h for n,h in mapping.items()),'Frozen source or fixture drift'


def main():
    fixtures={}
    for name in COHORTS:
        fixtures.update(json.loads((HERE.parent/name/'LOCK.json').read_text()))
    verify(fixtures)
    sources={str(p.relative_to(ROOT)):sha(p) for crate in ['plasm-agent-core','plasm-core'] for p in (ROOT/'crates'/crate/'src').rglob('*.rs')}
    sources.update({str(p.relative_to(ROOT)):sha(p) for p in HERE.iterdir() if p.is_file()})
    candidate={k:sha(p) for k,p in FILES.items()}
    for k,p in FILES.items():
        (OUT/f'{k}.candidate.rs').write_bytes(p.read_bytes())
    patch=''.join(''.join(difflib.unified_diff((OUT/f'{k}.baseline.rs').read_text().splitlines(True),p.read_text().splitlines(True),fromfile=str(p.relative_to(ROOT)),tofile=str(p.relative_to(ROOT)))) for k,p in FILES.items())
    (OUT/'candidate.patch').write_text(patch)
    with (OUT/'LOCK.json').open('x') as f:
        json.dump({'fixtures':fixtures,'sources':sources,'baseline':{k:sha(OUT/f'{k}.baseline.rs') for k in FILES},'candidate':candidate,
                   'baseline_audit_sha256':sha(ROOT/'eval-reports/retention-value-v1-20261030/AUDIT.json')},f,indent=2)
    phases=[]
    try:
        cmd=['bash','build/bazel/linux.sh','test','//crates/plasm-agent-core:unit_tests','--test_arg=discovery_matcher::','--test_output=all','--nocache_test_results']
        with (OUT/'MATCHER.log').open('x') as f:
            r=subprocess.run(cmd,cwd=ROOT,stdout=f,stderr=subprocess.STDOUT)
        phases.append({'phase':'matcher','exit':r.returncode})
        assert r.returncode==0,'Native invariant/preflight failure'
        for cohort in COHORTS:
            verify(fixtures);verify(sources)
            cmd=['python3',str(HERE.parent/cohort/'run.py'),'run',str(OUT/cohort)]
            with (OUT/f'{cohort}.log').open('x') as f:
                r=subprocess.run(cmd,cwd=ROOT,stdout=f,stderr=subprocess.STDOUT)
            phases.append({'phase':cohort,'exit':r.returncode})
            assert r.returncode==0,'Native harness fault; no automatic retry'
        verify(fixtures);verify(sources)
    finally:
        (OUT/'PHASES.json').write_text(json.dumps(phases,indent=2))
        assert all(sha(FILES[k])==h for k,h in candidate.items()),'Concurrent edit detected; refusing restoration'
        for k,p in FILES.items():p.write_bytes((OUT/f'{k}.baseline.rs').read_bytes())
        assert all(sha(p)==sha(OUT/f'{k}.baseline.rs') for k,p in FILES.items())
        (OUT/'RESTORED.json').write_text(json.dumps({'production_restored':True,'hashes':{k:sha(p) for k,p in FILES.items()}},indent=2))
    print('Candidate matrix completed; baseline production files restored')


if __name__=='__main__':main()
