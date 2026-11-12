"""Frozen model-free boundary stress, with guaranteed candidate restoration."""
import hashlib
import importlib.util
import json
import subprocess
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
OUT = ROOT / 'eval-reports/catalog-partition-v1-20261030'
PREV = ROOT / 'eval-reports/catalog-breadth-v1-20261030-r2'
FILES = {'connected': ROOT/'crates/plasm-agent-core/src/discovery_matcher/online/connected.rs',
         'online': ROOT/'crates/plasm-agent-core/src/discovery_matcher/online.rs'}
def sha(p): return hashlib.sha256(p.read_bytes()).hexdigest()
def verify(mapping):
    assert all(sha(ROOT / name) == value for name, value in mapping.items()), 'Frozen evidence drift'

def main():
    OUT.mkdir(exist_ok=False)
    prior = json.loads((PREV/'LOCK.json').read_text())
    assert all(sha(p) == prior['baseline'][k] for k,p in FILES.items())
    for k,p in FILES.items():
        (OUT/f'{k}.baseline.rs').write_bytes(p.read_bytes())
        (OUT/f'{k}.candidate.rs').write_bytes((PREV/f'{k}.candidate.rs').read_bytes())
    fixture_paths = list(HERE.glob('*')) + list((ROOT/'fixtures/schemas/discovery_trajectory_v4').glob('*.yaml'))
    fixed = {str(p.relative_to(ROOT)):sha(p) for p in fixture_paths if p.is_file()}
    for crate in ['plasm-agent-core','plasm-core']:
        fixed.update({str(p.relative_to(ROOT)):sha(p) for p in (ROOT/'crates'/crate/'src').rglob('*.rs') if p not in FILES.values()})
    lock = {'fixed':fixed,'baseline':{k:sha(p) for k,p in FILES.items()},'candidate':{k:sha(OUT/f'{k}.candidate.rs') for k in FILES}}
    (OUT/'LOCK.json').write_text(json.dumps(lock,indent=2))
    phases=[]
    try:
        for arm in ['baseline','candidate']:
            verify(fixed)
            for k,p in FILES.items(): p.write_bytes((OUT/f'{k}.{arm}.rs').read_bytes())
            dest=OUT/arm; dest.mkdir()
            cmd=['bash','build/bazel/linux.sh','test','//crates/plasm-agent-core:unit_tests','--test_arg=frozen_catalog_partition_stress_v1','--test_arg=--ignored','--test_arg=--nocapture','--test_output=all','--nocache_test_results','--test_env=PLASM_TRAJECTORY_REPORT=/workspace/'+str((dest/'RESULTS.json').relative_to(ROOT))]
            with (dest/'TEST.log').open('x') as log:
                result=subprocess.run(cmd,cwd=ROOT,stdout=log,stderr=subprocess.STDOUT)
            phases.append({'arm':arm,'exit':result.returncode})
            verify(fixed)
            assert all(sha(p)==lock[arm][k] for k,p in FILES.items())
            assert result.returncode==0,'Native harness fault; no automatic retry'
    finally:
        (OUT/'PHASES.json').write_text(json.dumps(phases,indent=2))
        assert all(sha(p) in [lock['baseline'][k],lock['candidate'][k]] for k,p in FILES.items()),'Concurrent edit: refuse overwrite'
        for k,p in FILES.items():p.write_bytes((OUT/f'{k}.baseline.rs').read_bytes())
        (OUT/'RESTORED.json').write_text(json.dumps({k:sha(p) for k,p in FILES.items()},indent=2))
    subprocess.run(['python3',str(HERE/'report.py')],cwd=ROOT,check=True)

if __name__=='__main__': main()
