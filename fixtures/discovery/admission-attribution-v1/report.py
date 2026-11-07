import html
import json
from run import ROOT,OUT,COHORTS,sha,verify

def subset(old,new,path=''):
    if isinstance(old,dict):
        for k,v in old.items():
            assert k in new,f'Missing {path}/{k}'
            subset(v,new[k],path+'/'+k)
    elif isinstance(old,list):
        assert len(old)==len(new),path
        for i,(a,b) in enumerate(zip(old,new)):subset(a,b,path+f'/{i}')
    else:assert old==new, f'Parity mismatch {path}'

def main():
    verify(json.loads((OUT/'LOCK.json').read_text()))
    rows=[]
    for cohort,(_,old_dir) in COHORTS.items():
        old=json.loads((ROOT/'eval-reports'/old_dir/'RESULTS.json').read_text())
        new=json.loads((OUT/cohort/'RESULTS.json').read_text())
        assert old['policy']==new['policy']
        def key(c):return(c['id'],c['distractors'],c['rotating'])
        prior={key(c):c for c in old['cases']}
        assert set(prior)=={key(c) for c in new['cases']}
        for c in new['cases']:
            subset(prior[key(c)]['turns'],c['turns'],cohort+'/'+c['id'])
            t=c['turns'][0];a=t['attribution']
            scores={(r['reference']['catalog'],r['reference']['capability']):r for r in a['scores']}
            targets=[]
            for s in t['status']:
                ref=s['target'];score=scores.get((ref['catalog'],ref['capability']))
                targets.append({'status':s,'score':score})
            rows.append({'cohort':cohort,'id':c['id'],'distractors':c['distractors'],'rotating':c['rotating'],
                'first_complete':next((t['search'] for t in c['turns'] if all(t['outcomes'])),None),
                'targets':targets,'top_ranked':a['scores'][:5],'admissions':a['admissions'],
                'packet_bytes':t['receipt']['input_bytes']})
    assert len(rows)==98
    audit={'variants':98,'historical_turn_parity':True,'policy_unchanged':True,'provider_calls':0,'rows':rows,
       'limits':'First-search attribution only; historical fields compared exactly, added fields allowed. Timings excluded. No caller execution or model quality measurement.'}
    (OUT/'AUDIT.json').write_text(json.dumps(audit,indent=2))
    lines=['# Native admission attribution','', '98/98 variants preserve all historical per-turn fields, including packets, receipts, decisions and outcomes. Production policy is unchanged. No provider calls.','',audit['limits'],'',
      '| Cohort/case | Growth | Rotate | First complete | Initial admissions | Packet bytes |','|---|---:|---|---:|---|---:|']
    for r in rows:
        kinds={k:sum(a['kind']==k for a in r['admissions']) for k in ['initial_root','optional_path','remaining_root']}
        lines.append(f"| {r['cohort']}/{r['id']} | {r['distractors']} | {r['rotating']} | {r['first_complete']} | {kinds} | {r['packet_bytes']} |")
    text='\n'.join(lines)+'\n';(OUT/'REPORT.md').write_text(text)
    (OUT/'REPORT.html').write_text('<!doctype html><meta charset="utf-8"><title>Admission attribution</title><style>body{max-width:1300px;margin:40px auto;font:16px system-ui}pre{white-space:pre-wrap;line-height:1.5}</style><pre>'+html.escape(text)+'</pre>')
    hashes={str(p.relative_to(OUT)):sha(p) for p in OUT.rglob('*') if p.is_file() and p!=OUT/'HASHES.json'}
    (OUT/'HASHES.json').write_text(json.dumps(hashes,indent=2));assert all(sha(OUT/n)==v for n,v in hashes.items())
    print('98 variants: exact historical turn parity and evidence hashes verified.')
if __name__=='__main__':main()
