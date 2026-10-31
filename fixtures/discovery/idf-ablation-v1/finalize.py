import html
import json
from run import ROOT,HERE,OUT,FILES,sha,verify
lock=json.loads((OUT/'LOCK.json').read_text());verify(lock['fixed'])
assert all(sha(p)==lock['baseline'][k] for k,p in FILES.items())
audit=json.loads((OUT/'AUDIT.json').read_text());assert audit['variants']==114 and not audit['adoption_screen']
phases=json.loads((OUT/'PHASES.json').read_text());assert len(phases)==7 and all(p['exit']==0 for p in phases)
findings=(HERE/'FINDINGS.md').read_text();(OUT/'FINDINGS.md').write_text(findings)
report=(OUT/'REPORT.md').read_text()
if not report.startswith(findings):report=findings+'\n---\n\n'+report
(OUT/'REPORT.md').write_text(report)
(OUT/'REPORT.html').write_text('<!doctype html><meta charset="utf-8"><title>Constant IDF: rejected</title><style>body{max-width:1250px;margin:40px auto;font:16px system-ui;background:#101820;color:#e7eef2}pre{white-space:pre-wrap;line-height:1.6}</style><pre>'+html.escape(report)+'</pre>')
hashes={str(p.relative_to(OUT)):sha(p) for p in OUT.rglob('*') if p.is_file() and p!=OUT/'HASHES.json'}
(OUT/'HASHES.json').write_text(json.dumps(hashes,indent=2));assert all(sha(OUT/n)==h for n,h in hashes.items())
print('114 paired variants, all phases, frozen inputs, restoration and report hashes verified.')
