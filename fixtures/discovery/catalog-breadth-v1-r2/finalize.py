"""Attach interpretation without changing the frozen comparison script."""
import hashlib
import html
import json
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
OUT = ROOT / 'eval-reports/catalog-breadth-v1-20261030-r2'
def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

lock = json.loads((OUT / 'LOCK.json').read_text())
for key, name in [('connected', 'online/connected.rs'), ('online', 'online.rs')]:
    assert sha(ROOT / 'crates/plasm-agent-core/src/discovery_matcher' / name) == lock['baseline'][key]
assert all(sha(ROOT / name) == value for name, value in lock['fixtures'].items())
phases = json.loads((OUT / 'PHASES.json').read_text())
assert len(phases) == 4 and all(p['exit'] == 0 for p in phases)
audit = json.loads((OUT / 'AUDIT.json').read_text())
assert len(audit['pairs']) == 58 and audit['development_signal']
findings = (HERE / 'FINDINGS.md').read_text()
(OUT / 'FINDINGS.md').write_text(findings)
report = (OUT / 'REPORT.md').read_text()
if not report.startswith(findings):
    report = findings + '\n---\n\n' + report
(OUT / 'REPORT.md').write_text(report)
(OUT / 'REPORT.html').write_text('<!doctype html><meta charset="utf-8"><title>Catalogue breadth experiment</title><style>body{max-width:1250px;margin:40px auto;background:#101820;color:#e7eef2;font:16px system-ui}pre{white-space:pre-wrap;line-height:1.6}</style><pre>' + html.escape(report) + '</pre>')
hashes = {str(p.relative_to(OUT)): sha(p) for p in OUT.rglob('*') if p.is_file() and p != OUT / 'HASHES.json'}
(OUT / 'HASHES.json').write_text(json.dumps(hashes, indent=2) + '\n')
assert all(sha(OUT / name) == value for name, value in hashes.items())
print('58 paired variants, frozen fixtures, production restoration and evidence hashes verified.')
