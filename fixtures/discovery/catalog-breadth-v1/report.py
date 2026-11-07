"""All-row operational comparison under the frozen acceptance rule."""
import html
import json
from run import HERE,ROOT,OUT,FILES,COHORTS,sha,verify


def main():
    lock=json.loads((OUT/'LOCK.json').read_text());verify(lock['fixtures'])
    assert sha(ROOT/'eval-reports/retention-value-v1-20261030/AUDIT.json')==lock['baseline_audit_sha256']
    assert all(sha(p)==lock['baseline'][k] for k,p in FILES.items())
    base=json.loads((ROOT/'eval-reports/retention-value-v1-20261030/AUDIT.json').read_text())
    old={(r['cohort'],r['id'],r['distractors'],r['rotating']):r for r in base['rows'] if r['arm']=='fixture_judgment'}
    pairs=[]
    for cohort in COHORTS:
        p=OUT/cohort/'SUMMARY.json'
        if not p.exists():continue
        for c in json.loads(p.read_text())['cases']:
            b=old[(cohort,c['id'],c['distractors'],c['rotating'])]
            pairs.append({'cohort':cohort,'id':c['id'],'distractors':c['distractors'],'rotating':c['rotating'],
              'before':b['first_complete'],'after':c['first_complete'],'baseline':b,'candidate':c,
              'delta_requests':c['requests_to_completion_or_horizon']-b['requests_to_completion_or_horizon'],
              'delta_bytes':c['bytes_to_completion_or_horizon']-b['bytes_to_completion_or_horizon'],
              'delta_declarations':c['declaration_bytes']-b['declaration_bytes']})
    groups={}
    for cohort in COHORTS:
        rows=[p for p in pairs if p['cohort']==cohort]
        groups[cohort]={'variants':len(rows),'before_pass':sum(p['baseline']['gate_pass'] for p in rows),'after_pass':sum(p['candidate']['gate_pass'] for p in rows),
          'requests_before':sum(p['baseline']['requests_to_completion_or_horizon'] for p in rows),
          'requests_after':sum(p['candidate']['requests_to_completion_or_horizon'] for p in rows),
          'bytes_delta':sum(p['delta_bytes'] for p in rows),'declarations_delta':sum(p['delta_declarations'] for p in rows)}
    recovered=[p for p in pairs if not p['baseline']['gate_pass'] and p['candidate']['gate_pass']]
    lost=[p for p in pairs if p['baseline']['gate_pass'] and not p['candidate']['gate_pass']]
    ordinary_ok=all(p['after']==1 for p in pairs if p['cohort']=='root-diversity-v1')
    excessive=[p for p in pairs if p['before'] is not None and (p['after'] is None or p['after']>p['before']+1)]
    g=groups['trajectory-v4'];half=g['requests_after']<=g['requests_before']/2
    accepted=len(pairs)==58 and len(recovered)==6 and not lost and ordinary_ok and not excessive and half
    audit={'development_signal':accepted,'production_restored':True,'groups':groups,'pairs':pairs,
      'recoveries':len(recovered),'lost_gates':len(lost),'ordinary_ok':ordinary_ok,'excessive_delays':len(excessive),'trajectory_requests_halved':half,
      'scope':'Model-free frozen development cohorts; no provider, adaptive agent, execution, holdout or full-task score.'}
    (OUT/'AUDIT.json').write_text(json.dumps(audit,indent=2)+'\n')
    lines=['# Catalogue breadth experiment','',f'Development signal: {accepted}. Production restored to baseline. Recovered gates: {len(recovered)}; lost gates: {len(lost)}.','',audit['scope'],'',
      '| Cohort | Availability before → after | Prefix requests before → after | Δ admitted bytes | Δ endpoint declaration bytes |','|---|---|---|---:|---:|']
    for cohort,g in groups.items():lines.append(f"| {cohort} | {g['before_pass']} → {g['after_pass']} / {g['variants']} | {g['requests_before']} → {g['requests_after']} | {g['bytes_delta']} | {g['declarations_delta']} |")
    lines+=['','Single-run timings are diagnostic; no speedup estimate. All original quality labels and budgets remain unchanged. Endpoint declaration bytes are not transmitted tokens.','',
      '| Case | Growth | Rotate | First complete before → after | Δ requests | Δ bytes | Δ declarations |','|---|---:|---|---|---:|---:|---:|']
    for p in pairs:lines.append(f"| {p['cohort']}/{p['id']} | {p['distractors']} | {p['rotating']} | {p['before']} → {p['after']} | {p['delta_requests']} | {p['delta_bytes']} | {p['delta_declarations']} |")
    text='\n'.join(lines)+'\n'
    (OUT/'REPORT.md').write_text(text)
    (OUT/'REPORT.html').write_text('<!doctype html><meta charset="utf-8"><title>Catalogue breadth</title><style>body{max-width:1300px;margin:40px auto;background:#101820;color:#e7eef2;font:16px system-ui}pre{white-space:pre-wrap;line-height:1.5}</style><pre>'+html.escape(text)+'</pre>')
    hashes={str(p.relative_to(OUT)):sha(p) for p in OUT.rglob('*') if p.is_file() and p!=OUT/'HASHES.json'}
    (OUT/'HASHES.json').write_text(json.dumps(hashes,indent=2));assert all(sha(OUT/n)==h for n,h in hashes.items())
    print(json.dumps({k:v for k,v in audit.items() if k!='pairs'}))


if __name__=='__main__':main()
