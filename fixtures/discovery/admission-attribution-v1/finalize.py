import html
import json
from run import HERE,OUT,ROOT,sha,verify

verify(json.loads((OUT/'LOCK.json').read_text()))
audit=json.loads((OUT/'AUDIT.json').read_text())
assert audit['variants']==98 and audit['historical_turn_parity']
findings=(HERE/'FINDINGS.md').read_text()
(OUT/'FINDINGS.md').write_text(findings)
report=(OUT/'REPORT.md').read_text()
if not report.startswith(findings):report=findings+'\n---\n\n'+report
(OUT/'REPORT.md').write_text(report)
(OUT/'REPORT.html').write_text('<!doctype html><meta charset="utf-8"><title>Admission attribution findings</title><style>body{max-width:1250px;margin:40px auto;font:16px system-ui;background:#101820;color:#e7eef2}pre{white-space:pre-wrap;line-height:1.6}</style><pre>'+html.escape(report)+'</pre>')
hashes={str(p.relative_to(OUT)):sha(p) for p in OUT.rglob('*') if p.is_file() and p!=OUT/'HASHES.json'}
(OUT/'HASHES.json').write_text(json.dumps(hashes,indent=2));assert all(sha(OUT/n)==h for n,h in hashes.items())
print('Attribution findings published; frozen source and evidence hashes verified.')
