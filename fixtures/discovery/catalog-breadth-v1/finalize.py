"""Record the preflight fault without changing the frozen experiment analysis."""
import hashlib
import html
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
OUT = ROOT / 'eval-reports/catalog-breadth-v1-20261030'

def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

lock = json.loads((OUT / 'LOCK.json').read_text())
for key, name in [('connected', 'online/connected.rs'), ('online', 'online.rs')]:
    assert sha(ROOT / 'crates/plasm-agent-core/src/discovery_matcher' / name) == lock['baseline'][key]
audit = json.loads((OUT / 'AUDIT.json').read_text())
assert not audit['pairs']
audit.update(status='invalid_preflight', completed_variants=0,
             development_signal=None, ordinary_ok=None,
             trajectory_requests_halved=None,
             reason='New proof assumes lexical.len() > 2; fixture fails that precondition before ordering is checked.',
             active_checks_passed=136, active_checks_failed=1)
(OUT / 'AUDIT.json').write_text(json.dumps(audit, indent=2) + '\n')
text = '''# Catalogue breadth: invalid preflight

The experiment stopped before any of the 58 frozen variants ran. No conclusion about catalogue scheduling is supported.

The new single-catalogue proof failed its setup assertion `lexical.len() > 2`, before checking ordering. The other 136 active matcher checks passed; 11 tests were ignored by this invocation. This is a test-construction fault, not a demonstrated scheduling defect.

Both production files were restored and their baseline hashes independently verified. No provider calls, execution effects or AppWorld evaluation occurred. Frozen sources, candidate patch, logs and original protocol remain preserved.

Next: correct the abstract proof setup, freeze a separately identified preflight repair with the identical scheduling candidate and acceptance criteria, then measure all 58 variants. Do not tune the candidate from this invalid attempt. Even a successful result would still need catalogue-sharding and irrelevant-catalogue controls before adoption.
'''
(OUT / 'REPORT.md').write_text(text)
(OUT / 'REPORT.html').write_text('<!doctype html><meta charset="utf-8"><title>Catalogue breadth: invalid preflight</title><style>body{max-width:1000px;margin:40px auto;font:17px system-ui}pre{white-space:pre-wrap;line-height:1.6}</style><pre>' + html.escape(text) + '</pre>')
hashes = {str(p.relative_to(OUT)): sha(p) for p in OUT.rglob('*') if p.is_file() and p.name != 'HASHES.json'}
(OUT / 'HASHES.json').write_text(json.dumps(hashes, indent=2) + '\n')
assert all(sha(OUT / name) == value for name, value in hashes.items())
print('Invalid preflight recorded; production restoration and evidence hashes verified.')
