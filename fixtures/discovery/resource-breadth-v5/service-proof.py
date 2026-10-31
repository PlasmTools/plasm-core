"""Run model-free ownership/continuation proofs on a disposable isolated DB."""
import argparse
import hashlib
import json
import subprocess
import time
import uuid
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('output', type=Path)
    out = parser.parse_args().output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    name = 'plasm-discovery-breadth-proof-' + uuid.uuid4().hex[:12]
    docker = lambda *args: subprocess.check_output(['docker', *args], text=True).strip()
    sources = [ROOT / 'crates/plasm-agent-core/src' / n for n in [
        'discovery_service.rs', 'discovery_service/concurrency_tests.rs', 'discovery_store.sql']]
    hashes = {str(p.relative_to(ROOT)): hashlib.sha256(p.read_bytes()).hexdigest() for p in sources}
    (out / 'MANIFEST.json').write_text(json.dumps({'sources': hashes, 'container': name, 'provider_calls': 0}, indent=2))
    docker('run', '-d', '--name', name, '-e', 'POSTGRES_PASSWORD=abstract-proof', '-p', '127.0.0.1::5432', 'pgvector/pgvector:pg16')
    try:
        for _ in range(60):
            if subprocess.run(['docker', 'exec', name, 'pg_isready', '-U', 'postgres'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0:
                break
            time.sleep(1)
        else:
            raise RuntimeError('Disposable proof DB unavailable')
        with (out / 'SCHEMA.log').open('w') as log:
            subprocess.run(['docker', 'exec', '-i', name, 'psql', '-U', 'postgres', '-v', 'ON_ERROR_STOP=1'], input=sources[2].read_bytes(), stdout=log, stderr=subprocess.STDOUT, check=True)
        port = docker('port', name, '5432/tcp').rsplit(':', 1)[1]
        command = ['bash', 'build/bazel/linux.sh', 'test', '//crates/plasm-agent-core:unit_tests',
                   '--test_arg=discovery_service::concurrency_tests::', '--test_arg=--ignored',
                   '--test_arg=--test-threads=1', '--nocache_test_results', '--test_output=all',
                   '--test_env=PLASM_TEST_POSTGRES_URL=postgresql://postgres:abstract-proof@host.docker.internal:' + port + '/postgres']
        with (out / 'TEST.log').open('w') as log:
            subprocess.run(command, cwd=ROOT, stdout=log, stderr=subprocess.STDOUT, check=True)
        assert '7 passed; 0 failed;' in (out / 'TEST.log').read_text(), 'Expected all seven ignored proofs'
        assert all(hashlib.sha256((ROOT / p).read_bytes()).hexdigest() == h for p, h in hashes.items()), 'Source drift'
    finally:
        docker('rm', '-fv', name)
        (out / 'CLEANUP.json').write_text(json.dumps({'removed_owned_disposable_container': name}))
    (out / 'HASHES.json').write_text(json.dumps({p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in out.iterdir() if p.is_file()}, indent=2))
    print('Seven PostgreSQL ownership/continuation proofs passed; disposable DB removed')


if __name__ == '__main__':
    main()
