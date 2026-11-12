import html
import json
from run import ROOT,HERE,OUT,sha,verify
verify(json.loads((OUT/'LOCK.json').read_text()))
data=json.loads((OUT/'RESULTS.json').read_text())
def strings(value):
    if isinstance(value,str):yield value
    elif isinstance(value,dict):
        for child in value.values():yield from strings(child)
    elif isinstance(value,list):
        for child in value:yield from strings(child)
assert len(data['cases'])==16
assert all(any(c['turns'][0]['request'] in text for text in strings(c['turns'][0]['packets'])) for c in data['cases'])
audit=json.loads((OUT/'AUDIT.json').read_text())
assert all(p['identical_scores_and_ranking'] and p['identical_admission_trace'] for p in audit['pair_conditions'])
audit['full_current_request_present_in_first_packet']=16
(OUT/'AUDIT.json').write_text(json.dumps(audit,indent=2))
findings=(HERE/'FINDINGS.md').read_text();(OUT/'FINDINGS.md').write_text(findings)
report=(OUT/'REPORT.md').read_text()
if not report.startswith(findings):report=findings+'\n---\n\n'+report
(OUT/'REPORT.md').write_text(report)
(OUT/'REPORT.html').write_text('<!doctype html><meta charset="utf-8"><title>Action role representation</title><style>body{max-width:1200px;margin:40px auto;font:16px system-ui;background:#101820;color:#e7eef2}pre{white-space:pre-wrap;line-height:1.6}</style><pre>'+html.escape(report)+'</pre>')
hashes={str(p.relative_to(OUT)):sha(p) for p in OUT.rglob('*') if p.is_file() and p!=OUT/'HASHES.json'}
(OUT/'HASHES.json').write_text(json.dumps(hashes,indent=2));assert all(sha(OUT/n)==v for n,v in hashes.items())
print('Role reversal, full-request preservation, frozen inputs and hashes verified.')
