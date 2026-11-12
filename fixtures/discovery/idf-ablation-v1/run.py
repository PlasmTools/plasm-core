import hashlib
import json
import re
import subprocess
from pathlib import Path

HERE=Path(__file__).resolve().parent
ROOT=HERE.parents[2]
OUT=ROOT/'eval-reports/idf-ablation-v1-20261030'
PREV=ROOT/'eval-reports/admission-attribution-v1-20261030'
FILES={'rank':ROOT/'crates/plasm-agent-core/src/discovery_matcher/search_rank.rs','policy':ROOT/'crates/plasm-agent-core/src/discovery_matcher/online.rs'}
TESTS={'trajectory-v4':'frozen_discovery_trajectory_v4','resource-breadth-v5':'frozen_discovery_resource_breadth_v5','root-diversity-v1':'frozen_discovery_root_counter_controls','catalog-partition-v1':'frozen_catalog_partition_stress_v1','rare-action':'frozen_rare_action_v1'}
def sha(p):return hashlib.sha256(p.read_bytes()).hexdigest()
def verify(m):assert all(sha(ROOT/n)==v for n,v in m.items()),'Frozen input drift'
def main():
    OUT.mkdir(exist_ok=False)
    prior=json.loads((PREV/'LOCK.json').read_text())
    # The sole changed old source is test registration; all production code is identical.
    verify({n:v for n,v in prior.items() if n.startswith('crates/') and not n.endswith('/online/trajectory_suite.rs')})
    fixed={str(p.relative_to(ROOT)):sha(p) for crate in ['plasm-agent-core','plasm-core'] for p in (ROOT/'crates'/crate/'src').rglob('*.rs') if p not in FILES.values()}
    fixed.update({n:v for n,v in prior.items() if n.startswith('fixtures/')})
    fixed.update({str(p.relative_to(ROOT)):sha(p) for p in HERE.iterdir() if p.is_file()})
    for cohort in list(TESTS)[:-1]:
        p=PREV/cohort/'RESULTS.json';fixed[str(p.relative_to(ROOT))]=sha(p)
    verify(fixed)
    for key,p in FILES.items():
        original=p.read_text();(OUT/f'{key}.baseline.rs').write_text(original)
        candidate=original
        if key=='rank':
            candidate,n=re.subn(r'let idf = .*?;', 'let idf = 1.0;',candidate,flags=re.S)
            assert n==2
            candidate=candidate.replace('            let n = self.lengths.len() as f64;\n','').replace('            let df = posting.len() as f64;\n','')
        else:
            candidate,n=re.subn(r'(const SEARCH_POLICY: &str =\s*)"[^"]+"',r'\1"constant-idf-diagnostic-16000-byte-v1"',candidate)
            assert n==1
        (OUT/f'{key}.candidate.rs').write_text(candidate)
    lock={'fixed':fixed,**{arm:{k:sha(OUT/f'{k}.{arm}.rs') for k in FILES} for arm in ['baseline','candidate']}}
    (OUT/'LOCK.json').write_text(json.dumps(lock,indent=2))
    phases=[]
    try:
        for arm in ['baseline','candidate']:
            for k,p in FILES.items():p.write_bytes((OUT/f'{k}.{arm}.rs').read_bytes())
            tests={'rare-action':TESTS['rare-action']} if arm=='baseline' else {'matcher':'discovery_matcher::',**TESTS}
            for cohort,test in tests.items():
                verify(fixed)
                dest=OUT/arm/cohort;dest.mkdir(parents=True)
                cmd=['bash','build/bazel/linux.sh','test','//crates/plasm-agent-core:unit_tests','--test_arg='+test,'--test_output=all','--nocache_test_results']
                if cohort!='matcher':cmd+=['--test_arg=--ignored','--test_arg=--nocapture','--test_env=PLASM_TRAJECTORY_REPORT=/workspace/'+str((dest/'RESULTS.json').relative_to(ROOT))]
                with (dest/'TEST.log').open('x') as f:r=subprocess.run(cmd,cwd=ROOT,stdout=f,stderr=subprocess.STDOUT)
                phases.append({'arm':arm,'cohort':cohort,'exit':r.returncode})
                (OUT/'PHASES.json').write_text(json.dumps(phases,indent=2))
                verify(fixed);assert all(sha(p)==lock[arm][k] for k,p in FILES.items())
                assert r.returncode==0,'Native integrity fault; no automatic retry'
    finally:
        assert all(sha(p) in [lock['baseline'][k],lock['candidate'][k]] for k,p in FILES.items()),'Concurrent edit: refusing overwrite'
        for k,p in FILES.items():p.write_bytes((OUT/f'{k}.baseline.rs').read_bytes())
        assert all(sha(p)==lock['baseline'][k] for k,p in FILES.items())
        (OUT/'RESTORED.json').write_text(json.dumps({'restored':True,'hashes':lock['baseline']},indent=2))
    subprocess.run(['python3',str(HERE/'report.py')],cwd=ROOT,check=True)
if __name__=='__main__':main()
