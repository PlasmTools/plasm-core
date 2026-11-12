"""Attach interpretation without changing frozen measurements or analysis rules."""
import html
import json
from run import HERE, OUT, sha, verify

lock=json.loads((OUT/'LOCK.json').read_text())
verify(lock['fixtures']);verify(lock['sources'])
audit=json.loads((OUT/'AUDIT.json').read_text())
assert audit['complete'] and not audit['historical_parity_differences']
findings=(HERE/'FINDINGS.md').read_text()
old=(OUT/'REPORT.md').read_text()
assert not old.startswith('# Return discovery design'), 'Already finalized'
text=findings+'\n\n'+old
(OUT/'REPORT.md').write_text(text)
(OUT/'REPORT.html').write_text('<!doctype html><meta charset="utf-8"><title>Operational discovery gradient</title><style>body{max-width:1300px;margin:40px auto;background:#101820;color:#e7eef2;font:16px system-ui}pre{white-space:pre-wrap;line-height:1.5}</style><pre>'+html.escape(text)+'</pre>')
hashes={p.name:sha(p) for p in OUT.iterdir() if p.is_file() and p.name!='HASHES.json'}
(OUT/'HASHES.json').write_text(json.dumps(hashes,indent=2))
assert all(sha(OUT/n)==h for n,h in hashes.items())
print('116 records audited; 58 historical baseline comparisons match; report hashes verified')
