"""Offline native discovery gradient. Never invokes a provider or an agent."""
import argparse
import hashlib
import json
import subprocess
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
TEST_NAME = 'frozen_discovery_trajectory_v4'


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def fixture_files():
    return [HERE / 'cases.json', HERE / 'PROTOCOL.md', *sorted(
        (ROOT / 'fixtures/schemas/discovery_trajectory_v4').glob('*.yaml'))]


def verify():
    lock = json.loads((HERE / 'LOCK.json').read_text())
    assert all(digest(ROOT / name) == value for name, value in lock.items()), 'Frozen fixture drift'
    return lock


def summarize(data):
    rows = []
    for case in data['cases']:
        turns = case['turns']
        first = next((t['search'] for t in turns if all(t['outcomes'])), None)
        prefix = turns[:first] if first is not None else turns
        rows.append({
            'id': case['id'], 'distractors': case['distractors'], 'rotating': case['rotating'],
            'first_complete': first,
            'first_per_outcome': [next((t['search'] for t in turns if t['outcomes'][i]), None)
                                  for i in range(len(turns[0]['outcomes']))],
            'gate_pass': first is not None and first <= 4,
            'missing_outcome_searches': sum(sum(not v for v in t['outcomes']) for t in turns),
            'no_gain_before_completion': sum(not t['newly_exposed'] for t in prefix),
            'requests_to_completion_or_horizon': sum(t['receipt']['requests'] for t in prefix),
            'bytes_to_completion_or_horizon': sum(t['receipt']['input_bytes'] for t in prefix),
            'post_completion_requests': sum(t['receipt']['requests'] for t in turns[len(prefix):]),
            'repeated_rejections_before_completion': sum(t['repeated_rejections'] for t in prefix),
            'journal_targets_before_completion': sum(t['journal_targets'] for t in prefix),
            'max_packet_bytes': max((p['bytes'] for t in turns for p in t['packets']), default=0),
            'declaration_bytes': prefix[-1]['declaration_bytes'],
            'gate_status': turns[3]['status'], 'final_status': turns[-1]['status'],
            'relation_visible_by_gate': turns[3]['relation_visible'],
            'timing': case['timing'],
        })
    failures = [{k: row[k] for k in ['id', 'distractors', 'rotating']} for row in rows if not row['gate_pass']]
    return {'development_only': True, 'provider_calls': 0, 'provider_usage': None,
            'quality_failures': failures, 'cases': rows,
            'limitation': 'Fixture judgment and scripted requests isolate mechanics, not model accuracy or agent execution.'}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('mode', choices=['freeze', 'verify', 'run', 'quality'])
    parser.add_argument('path', nargs='?')
    args = parser.parse_args()
    if args.mode == 'freeze':
        with (HERE / 'LOCK.json').open('x') as handle:
            json.dump({str(p.relative_to(ROOT)): digest(p) for p in fixture_files()}, handle, indent=2)
        print('Fixtures and labels frozen before baseline measurement')
        return
    lock = verify()
    if args.mode == 'verify':
        print('Frozen fixtures verified')
        return
    if args.mode == 'quality':
        result = summarize(json.loads(Path(args.path).read_text()))
        print(json.dumps({'quality_failures': result['quality_failures']}, indent=2))
        raise SystemExit(bool(result['quality_failures']))
    out = Path(args.path).resolve()
    out.relative_to(ROOT)  # Linux build container mounts this root at /workspace.
    out.mkdir(parents=True, exist_ok=False)
    sources = sorted(p for crate in ['plasm-agent-core', 'plasm-core']
                     for p in (ROOT / 'crates' / crate / 'src').rglob('*.rs'))
    sources += [HERE / 'run.py']
    hashes = {str(p.relative_to(ROOT)): digest(p) for p in sources}
    (out / 'MANIFEST.json').write_text(json.dumps({'fixtures': lock, 'sources': hashes,
        'head': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip()}, indent=2))
    output = '/workspace/' + str((out / 'RESULTS.json').relative_to(ROOT))
    command = ['bash', 'build/bazel/linux.sh', 'test', '//crates/plasm-agent-core:unit_tests',
               '--test_arg=' + TEST_NAME, '--test_arg=--ignored',
               '--test_arg=--nocapture', '--test_output=all', '--nocache_test_results',
               '--test_env=PLASM_TRAJECTORY_REPORT=' + output]
    with (out / 'TEST.log').open('w') as log:
        result = subprocess.run(command, cwd=ROOT, stdout=log, stderr=subprocess.STDOUT)
    verify()
    assert all(digest(ROOT / name) == value for name, value in hashes.items()), 'Source drift'
    if result.returncode:
        raise SystemExit('Native harness failed; inspect ' + str(out / 'TEST.log'))
    summary = summarize(json.loads((out / 'RESULTS.json').read_text()))
    (out / 'SUMMARY.json').write_text(json.dumps(summary, indent=2))
    report = ['# Discovery trajectory v4 baseline', '', summary['limitation'], '',
              '| Case | Distractors | Rotating | First complete | Missing-outcome searches | No-gain prefix | Gate |',
              '|---|---:|---|---:|---:|---:|---|']
    for row in summary['cases']:
        report.append(f"| {row['id']} | {row['distractors']} | {row['rotating']} | {row['first_complete']} | {row['missing_outcome_searches']} | {row['no_gain_before_completion']} | {'PASS' if row['gate_pass'] else 'FAIL'} |")
    report += ['', f"Quality failures: {len(summary['quality_failures'])}/{len(summary['cases'])}. Native integrity/permutation checks passed. No provider calls.",
               'If no quality failures occur, this fixture does not reproduce the observed starvation; do not claim full-agent readiness.']
    (out / 'REPORT.md').write_text('\n'.join(report) + '\n')
    (out / 'HASHES.json').write_text(json.dumps({p.name: digest(p) for p in out.iterdir() if p.is_file()}, indent=2))
    print(json.dumps({'output': str(out), 'quality_failures': summary['quality_failures']}))


if __name__ == '__main__':
    main()
