import hashlib
import json
import subprocess
from pathlib import Path
HERE=Path(__file__).resolve().parent
ROOT=HERE.parents[2]
OUT=ROOT/'eval-reports/action-role-pairs-v1-20261030'
def sha(p):return hashlib.sha256(p.read_bytes()).hexdigest()
def verify(m):assert all(sha(ROOT/n)==h for n,h in m.items()),'Source or fixture drift'
def main():
    OUT.mkdir(exist_ok=False)
    fixed={str(p.relative_to(ROOT)):sha(p) for p in HERE.iterdir() if p.is_file()}
    for crate in ['plasm-agent-core','plasm-core']:
        fixed.update({str(p.relative_to(ROOT)):sha(p) for p in (ROOT/'crates'/crate/'src').rglob('*.rs')})
    fixed.update({str(p.relative_to(ROOT)):sha(p) for p in (ROOT/'fixtures/schemas/discovery_trajectory_v4').glob('*.yaml')})
    baseline=json.loads((ROOT/'eval-reports/idf-ablation-v1-20261030/LOCK.json').read_text())['baseline']
    assert sha(ROOT/'crates/plasm-agent-core/src/discovery_matcher/search_rank.rs')==baseline['rank']
    assert sha(ROOT/'crates/plasm-agent-core/src/discovery_matcher/online.rs')==baseline['policy']
    (OUT/'LOCK.json').write_text(json.dumps(fixed,indent=2))
    cmd=['bash','build/bazel/linux.sh','test','//crates/plasm-agent-core:unit_tests','--test_arg=frozen_action_role_pairs_v1','--test_arg=--ignored','--test_arg=--nocapture','--test_output=all','--nocache_test_results','--test_env=PLASM_TRAJECTORY_REPORT=/workspace/'+str((OUT/'RESULTS.json').relative_to(ROOT))]
    with (OUT/'TEST.log').open('x') as f:r=subprocess.run(cmd,cwd=ROOT,stdout=f,stderr=subprocess.STDOUT)
    verify(fixed);assert r.returncode==0,'Native harness fault; no automatic retry'
    subprocess.run(['python3',str(HERE/'report.py')],cwd=ROOT,check=True)
if __name__=='__main__':main()
