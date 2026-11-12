import html
import importlib.util
import json
from run import ROOT,HERE,OUT,PREV,FILES,TESTS,sha,verify
def main():
    lock=json.loads((OUT/'LOCK.json').read_text());verify(lock['fixed'])
    assert all(sha(p)==lock['baseline'][k] for k,p in FILES.items())
    spec=importlib.util.spec_from_file_location('trajectory',HERE.parent/'trajectory-v4/run.py');m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
    pairs=[]
    for cohort in TESTS:
        old=(OUT/'baseline'/cohort if cohort=='rare-action' else PREV/cohort)/'RESULTS.json'
        new=OUT/'candidate'/cohort/'RESULTS.json'
        before=m.summarize(json.loads(old.read_text()))['cases'];after=m.summarize(json.loads(new.read_text()))['cases']
        key=lambda c:(c['id'],c['distractors'],c['rotating'])
        base={key(r):r for r in before};assert set(base)=={key(r) for r in after}
        pairs += [{'cohort':cohort,'before':base[key(r)],'after':r} for r in after]
    assert len(pairs)==114
    lost=[p for p in pairs if p['before']['gate_pass'] and not p['after']['gate_pass']]
    delayed=[p for p in pairs if p['before']['first_complete'] is not None and (p['after']['first_complete'] is None or p['after']['first_complete']>p['before']['first_complete']+1)]
    ordinary=all(p['after']['first_complete']==1 for p in pairs if p['cohort']=='root-diversity-v1')
    fresh_lost=[p for p in pairs if p['cohort']=='rare-action' and p['before']['first_complete']==1 and p['after']['first_complete']!=1]
    recovered=[p for p in pairs if p['cohort']=='trajectory-v4' and not p['before']['gate_pass'] and p['after']['gate_pass']]
    screen=len(recovered)==6 and not lost and not delayed and ordinary and not fresh_lost
    totals={c:{arm:{'passes':sum(p[arm]['gate_pass'] for p in pairs if p['cohort']==c),'requests':sum(p[arm]['requests_to_completion_or_horizon'] for p in pairs if p['cohort']==c)} for arm in ['before','after']} for c in TESTS}
    audit={'variants':114,'adoption_screen':screen,'lost_gates':len(lost),'excessive_delays':len(delayed),'fresh_one_search_losses':len(fresh_lost),'old_starvation_recoveries':len(recovered),'totals':totals,'pairs':pairs,'production_restored':True,'provider_calls':0}
    (OUT/'AUDIT.json').write_text(json.dumps(audit,indent=2))
    lines=['# Constant-IDF diagnostic','',f'Adoption screen: {screen}; old starvation recoveries {len(recovered)}/6; lost baseline gates {len(lost)}; excessive delays {len(delayed)}; fresh one-search losses {len(fresh_lost)}. Production restored.','',
    '98 baseline traces reused from exact-parity attribution; 16 fresh baseline controls and all 114 candidate variants measured. Fixed budgets, no model calls. Development diagnostics, not a holdout or agent score.','',json.dumps(totals,indent=2),'',
    '| Cohort/case | Growth | Rotate | Completion before → after | Requests | Bytes | Declarations |','|---|---:|---|---|---|---|---|']
    for p in pairs:
        b,a=p['before'],p['after'];lines.append(f"| {p['cohort']}/{a['id']} | {a['distractors']} | {a['rotating']} | {b['first_complete']} → {a['first_complete']} | {b['requests_to_completion_or_horizon']} → {a['requests_to_completion_or_horizon']} | {b['bytes_to_completion_or_horizon']} → {a['bytes_to_completion_or_horizon']} | {b['declaration_bytes']} → {a['declaration_bytes']} |")
    text='\n'.join(lines)+'\n';(OUT/'REPORT.md').write_text(text)
    (OUT/'REPORT.html').write_text('<!doctype html><meta charset="utf-8"><title>IDF ablation</title><style>body{max-width:1300px;margin:40px auto;font:16px system-ui}pre{white-space:pre-wrap;line-height:1.5}</style><pre>'+html.escape(text)+'</pre>')
    hashes={str(p.relative_to(OUT)):sha(p) for p in OUT.rglob('*') if p.is_file() and p!=OUT/'HASHES.json'}
    (OUT/'HASHES.json').write_text(json.dumps(hashes,indent=2));assert all(sha(OUT/n)==v for n,v in hashes.items())
    print(json.dumps({k:v for k,v in audit.items() if k!='pairs'}))
if __name__=='__main__':main()
