"""Freeze and run one model-free operational ablation; never call a provider."""
import hashlib
import json
import subprocess
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
OUT = ROOT / 'eval-reports/retention-value-v1-20261030'
COHORTS = ['trajectory-v4', 'resource-breadth-v5', 'root-diversity-v1']


def sha(p):
    return hashlib.sha256(p.read_bytes()).hexdigest()


def verify(mapping):
    assert all(sha(ROOT / n) == h for n, h in mapping.items()), 'Frozen drift'


def main():
    locks = {}
    for cohort in COHORTS:
        lock = json.loads((HERE.parent / cohort / 'LOCK.json').read_text())
        verify(lock)
        locks.update(lock)
    # Ensure the copied growth algorithm remains byte-identical, not a new recipe.
    src = ROOT / 'crates/plasm-agent-core/src/discovery_matcher/online'
    def growth(p):
        s = p.read_text(); return s[s.index('fn grown('):s.index('\n#[tokio::test]')].strip()
    assert growth(src / 'resource_growth.rs') == growth(src / 'retention_value.rs')
    sources = {str(p.relative_to(ROOT)):sha(p) for crate in ['plasm-agent-core','plasm-core'] for p in (ROOT/'crates'/crate/'src').rglob('*.rs')}
    sources.update({str(p.relative_to(ROOT)):sha(p) for p in HERE.iterdir() if p.is_file()})
    OUT.mkdir(parents=True,exist_ok=False)
    (OUT/'LOCK.json').write_text(json.dumps({'fixtures':locks,'sources':sources},indent=2))
    command = ['bash','build/bazel/linux.sh','test','//crates/plasm-agent-core:unit_tests',
               '--test_arg=frozen_retention_value_v1','--test_arg=--ignored','--test_arg=--nocapture',
               '--test_output=all','--nocache_test_results','--test_timeout=1800',
               '--test_env=PLASM_RETENTION_REPORT=/workspace/'+str((OUT/'RESULTS.jsonl').relative_to(ROOT))]
    with (OUT/'TEST.log').open('x') as f:
        result = subprocess.run(command,cwd=ROOT,stdout=f,stderr=subprocess.STDOUT)
    (OUT/'EXIT.json').write_text(json.dumps({'code':result.returncode}))
    verify(locks);verify(sources)
    assert result.returncode==0,'Harness failed; preserve partial records, no automatic retry'
    print('Both arms completed all 58 variants; insertion parity passed; zero provider calls')


if __name__ == '__main__':
    main()
