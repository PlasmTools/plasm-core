import hashlib
import json
import subprocess
from pathlib import Path

HERE=Path(__file__).resolve().parent
ROOT=HERE.parents[2]
OUT=ROOT/'eval-reports/admission-attribution-v1-20261030'
COHORTS={
 'trajectory-v4':('frozen_discovery_trajectory_v4','trajectory-v4-harness-parity-20261030'),
 'resource-breadth-v5':('frozen_discovery_resource_breadth_v5','resource-breadth-v5-baseline-r2'),
 'root-diversity-v1':('frozen_discovery_root_counter_controls','root-diversity-v1-baseline'),
 'catalog-partition-v1':('frozen_catalog_partition_stress_v1','catalog-partition-v1-20261030/baseline')}
def sha(p):return hashlib.sha256(p.read_bytes()).hexdigest()
def verify(files):assert all(sha(ROOT/n)==v for n,v in files.items()),'Frozen input drift'
def main():
    OUT.mkdir(exist_ok=False)
    fixed={}
    for cohort in list(COHORTS)[:3]:fixed.update(json.loads((HERE.parent/cohort/'LOCK.json').read_text()))
    partition=json.loads((ROOT/'eval-reports/catalog-partition-v1-20261030/LOCK.json').read_text())['fixed']
    fixed.update({n:v for n,v in partition.items() if n.startswith('fixtures/')})
    verify(fixed)
    for crate in ['plasm-agent-core','plasm-core']:
        fixed.update({str(p.relative_to(ROOT)):sha(p) for p in (ROOT/'crates'/crate/'src').rglob('*.rs')})
    fixed.update({str(p.relative_to(ROOT)):sha(p) for p in HERE.iterdir() if p.is_file()})
    for _,old in COHORTS.values():
        p=ROOT/'eval-reports'/old/'RESULTS.json';fixed[str(p.relative_to(ROOT))]=sha(p)
    (OUT/'LOCK.json').write_text(json.dumps(fixed,indent=2))
    phases=[]
    for name,test in [('matcher','discovery_matcher::')]+[(k,v[0]) for k,v in COHORTS.items()]:
        verify(fixed)
        dest=OUT/name;dest.mkdir()
        cmd=['bash','build/bazel/linux.sh','test','//crates/plasm-agent-core:unit_tests','--test_arg='+test,'--test_output=all','--nocache_test_results']
        if name!='matcher':cmd+=['--test_arg=--ignored','--test_arg=--nocapture','--test_env=PLASM_TRAJECTORY_REPORT=/workspace/'+str((dest/'RESULTS.json').relative_to(ROOT))]
        with (dest/'TEST.log').open('x') as f:r=subprocess.run(cmd,cwd=ROOT,stdout=f,stderr=subprocess.STDOUT)
        phases.append({'phase':name,'exit':r.returncode});(OUT/'PHASES.json').write_text(json.dumps(phases,indent=2))
        verify(fixed)
        assert r.returncode==0,'Harness fault; no automatic retry'
    subprocess.run(['python3',str(HERE/'report.py')],cwd=ROOT,check=True)
if __name__=='__main__':main()
