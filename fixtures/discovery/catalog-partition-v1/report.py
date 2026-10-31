"""Frozen all-case analysis, no exclusions or post-result thresholds."""
import html
import importlib.util
import json
from run import ROOT,HERE,OUT,FILES,sha,verify

def main():
    lock=json.loads((OUT/'LOCK.json').read_text());verify(lock['fixed'])
    assert all(sha(p)==lock['baseline'][k] for k,p in FILES.items())
    spec=importlib.util.spec_from_file_location('trajectory',HERE.parent/'trajectory-v4/run.py')
    module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
    summaries={}
    for arm in ['baseline','candidate']:
        data=json.loads((OUT/arm/'RESULTS.json').read_text())
        summaries[arm]=module.summarize(data)
        (OUT/arm/'SUMMARY.json').write_text(json.dumps(summaries[arm],indent=2))
    old={(r['id'],r['rotating']):r for r in summaries['baseline']['cases']}
    pairs=[{'id':r['id'],'rotating':r['rotating'],'before':old[(r['id'],r['rotating'])],'after':r} for r in summaries['candidate']['cases']]
    assert len(pairs)==40 and len(old)==40
    losses=[p for p in pairs if p['before']['gate_pass'] and not p['after']['gate_pass']]
    delays=[p for p in pairs if p['before']['first_complete'] is not None and (p['after']['first_complete'] is None or p['after']['first_complete']>p['before']['first_complete']+1)]
    sensitivity={}
    for arm in ['before','after']:
        groups={}
        for p in pairs:
            family,partition,case=p['id'].split('/')
            groups.setdefault(f'{family}/{case}/rotate={p["rotating"]}',[]).append(p[arm]['first_complete'] or 25)
        sensitivity[arm]={k:max(v)-min(v) for k,v in groups.items()}
    signal=not losses and not delays and max(sensitivity['after'].values())<=1
    totals={arm:{'passes':sum(p[arm]['gate_pass'] for p in pairs),'requests':sum(p[arm]['requests_to_completion_or_horizon'] for p in pairs),'bytes':sum(p[arm]['bytes_to_completion_or_horizon'] for p in pairs),'declarations':sum(p[arm]['declaration_bytes'] for p in pairs)} for arm in ['before','after']}
    audit={'adoption_screen_pass':signal,'variants':40,'lost_gates':len(losses),'excessive_delays':len(delays),'sensitivity':sensitivity,'totals':totals,'pairs':pairs,'production_restored':True,'provider_calls':0,'scope':'Controlled development stress, fixture judgment, no adaptive caller or execution.'}
    (OUT/'AUDIT.json').write_text(json.dumps(audit,indent=2))
    lines=['# Catalogue partition stress','',f'Adoption screen: {signal}. Lost baseline gates: {len(losses)}. Delays exceeding one search: {len(delays)}. Production restored.','',audit['scope'],'',
      'The same operations and descriptions are merged into workshop or partitioned into 1, 4, 16 or 120 other catalogues. Useful operations remain concentrated in workshop. All budgets and labels are fixed.','',
      '## Totals','',json.dumps(totals,indent=2),'','## Boundary sensitivity','',json.dumps(sensitivity,indent=2),'',
      '| Case | Rotating | First complete baseline → candidate | Requests | Admitted bytes | Declaration bytes |','|---|---|---|---|---|---|']
    for p in pairs:
        b,a=p['before'],p['after']
        lines.append(f"| {p['id']} | {p['rotating']} | {b['first_complete']} → {a['first_complete']} | {b['requests_to_completion_or_horizon']} → {a['requests_to_completion_or_horizon']} | {b['bytes_to_completion_or_horizon']} → {a['bytes_to_completion_or_horizon']} | {b['declaration_bytes']} → {a['declaration_bytes']} |")
    lines+=['','A missing completion remains a failure; sensitivity alone maps it to horizon+1. Packet bytes are not provider tokens. No full-agent readiness, holdout or runtime speedup claim.']
    text='\n'.join(lines)+'\n'
    (OUT/'REPORT.md').write_text(text)
    (OUT/'REPORT.html').write_text('<!doctype html><meta charset="utf-8"><title>Catalogue partition stress</title><style>body{max-width:1300px;margin:40px auto;font:16px system-ui;background:#101820;color:#e7eef2}pre{white-space:pre-wrap;line-height:1.5}</style><pre>'+html.escape(text)+'</pre>')
    hashes={str(p.relative_to(OUT)):sha(p) for p in OUT.rglob('*') if p.is_file() and p!=OUT/'HASHES.json'}
    (OUT/'HASHES.json').write_text(json.dumps(hashes,indent=2));assert all(sha(OUT/n)==h for n,h in hashes.items())
    print(json.dumps({k:v for k,v in audit.items() if k!='pairs'}))

if __name__=='__main__':main()
