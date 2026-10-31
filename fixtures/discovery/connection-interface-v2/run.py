"""Freeze, validate, run once, and audit the native fixed-packet diagnostic."""
import argparse
import hashlib
import json
import os
import shlex
import subprocess
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
OUT = ROOT / 'eval-reports/connection-interface-v2-20261030'
TEST = 'discovery_matcher::online::decision_diagnostic::connection_diagnostic::frozen_connection_interface_v2'


def sha(p):
    return hashlib.sha256(p.read_bytes()).hexdigest()


def save(p, value):
    with p.open('x') as f:
        json.dump(value, f, indent=2)
        f.write('\n')


def verify(mapping):
    for name, expected in mapping.items():
        assert sha(ROOT / name) == expected, f'Frozen drift: {name}'


def inputs():
    return [HERE / n for n in ['generate.py', 'cases.json', 'labels.json', 'PROTOCOL.md']] + [HERE.parent / 'decision-interface-v1/generate.py'] + sorted((ROOT / 'fixtures/schemas/discovery_decision_v1').glob('connection-v2-*/*.yaml'))


def sources():
    return {str(p.relative_to(ROOT)): sha(p) for p in [HERE / 'run.py'] + sorted(
        p for crate in ['plasm-agent-core', 'plasm-core'] for p in (ROOT / 'crates' / crate / 'src').rglob('*.rs'))}


def native(mode, filter_name=TEST):
    log = OUT / f'{mode}.log'
    args = ['bash', 'build/bazel/linux.sh', 'test', '//crates/plasm-agent-core:unit_tests',
            '--test_arg=' + filter_name, '--test_output=all', '--nocache_test_results']
    if mode != 'matcher':
        args += ['--test_arg=--ignored', '--test_arg=--nocapture',
                 '--test_env=PLASM_DECISION_DIAGNOSTIC_MODE=' + mode,
                 '--test_env=PLASM_DECISION_DIAGNOSTIC_OUT=/workspace/' + str(OUT.relative_to(ROOT))]
    with log.open('x') as f:
        result = subprocess.run(args, cwd=ROOT, stdout=f, stderr=subprocess.STDOUT)
    save(OUT / f'{mode}-EXIT.json', {'exit_code': result.returncode})
    assert result.returncode == 0, f'{mode} failed; preserved {log}'


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('mode', choices=['prepare', 'live', 'verify'])
    mode = parser.parse_args().mode
    if mode == 'prepare':
        save(HERE / 'LOCK.json', {str(p.relative_to(ROOT)): sha(p) for p in inputs()})
        OUT.mkdir(parents=True, exist_ok=False)
        save(OUT / 'SOURCES.json', sources())
    verify(json.loads((HERE / 'LOCK.json').read_text()))
    verify(json.loads((OUT / 'SOURCES.json').read_text()))
    if mode == 'prepare':
        native('export')
        packets = json.loads((OUT / 'PACKETS.json').read_text())
        assert len(packets) == 48 and len({p['id'] for p in packets}) == 48
        assert all(len(p['body'].encode()) == p['bytes'] <= 16000 for p in packets)
        assert all(hashlib.sha256(p['body'].encode()).hexdigest() == p['sha256'] for p in packets)
        for pair in dict.fromkeys(p['pair'] for p in packets):
            arms = {p['arm']: json.loads(p['body']) for p in packets if p['pair'] == pair}
            for local in arms['inline']['state']['question_evidence'].values():
                lines = local.pop('verified_connections', [])
                assert len(lines) == len(local['binding_refs'])
            assert arms['inline'] == arms['baseline'], 'Unexpected experimental delta'
        native('matcher', 'discovery_matcher::')
        verify(json.loads((OUT / 'SOURCES.json').read_text()))
        save(OUT / 'PACKET-LOCK.json', {'sha256': sha(OUT / 'PACKETS.json')})
        save(OUT / 'READY.json', {'packets': 48, 'provider_calls': 0, 'native_preflight': 'passed'})
        print('48 native packets validated and frozen; matcher invariants passed; zero model calls')
    elif mode == 'live':
        assert json.loads((OUT / 'READY.json').read_text())['native_preflight'] == 'passed'
        assert sha(OUT / 'PACKETS.json') == json.loads((OUT / 'PACKET-LOCK.json').read_text())['sha256']
        credential = os.environ.get('OPENROUTER_API_KEY')
        if not credential:
            for line in (ROOT.parent / '.env').read_text().splitlines():
                if line.strip().removeprefix('export ').startswith('OPENROUTER_API_KEY='):
                    values = shlex.split(line.split('=', 1)[1], comments=True)
                    assert len(values) == 1
                    credential = values[0]
        assert credential, 'Configured provider credential unavailable'
        with (OUT / 'LAUNCHED').open('x') as f:
            f.write('single authorized 48-packet diagnostic\n')
        private = OUT / 'credential.private'
        try:
            fd = os.open(private, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            with os.fdopen(fd, 'w') as f:
                f.write(credential)
            native('live')
        finally:
            private.unlink(missing_ok=True)
            save(OUT / 'CLEANUP.json', {'private_credential_removed': not private.exists()})
        verify(json.loads((OUT / 'SOURCES.json').read_text()))
        print('Authorized diagnostic completed; no effects or AppWorld run')
    else:
        print('Fixture and native source hashes verified')


if __name__ == '__main__':
    main()
