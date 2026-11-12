import html
import json
from run import HERE,ROOT,OUT,sha,verify
def main():
    verify(json.loads((OUT/'LOCK.json').read_text()))
    data=json.loads((OUT/'RESULTS.json').read_text());assert len(data['cases'])==16
    rows=[];pairs={}
    for c in data['cases']:
        t=c['turns'][0];desired=t['status'][0];first=next((t['search'] for t in c['turns'] if all(t['outcomes'])),None)
        prefix=c['turns'][:first] if first else c['turns']
        admitted=[]
        for a in t['attribution']['admissions']:
            admitted.extend([a['root']] if 'root' in a else [m['reference'] for m in a['members']])
        row={'id':c['id'],'growth':c['distractors'],'desired':desired,'first_complete':first,'first_admitted':admitted,
             'requests':sum(t['receipt']['requests'] for t in prefix),'bytes':sum(t['receipt']['input_bytes'] for t in prefix)}
        rows.append(row);pair,side=c['id'].split('/');pairs.setdefault((pair,c['distractors']),{})[side]=c
    pair_rows=[]
    for (name,growth),p in pairs.items():
        l,r=p['left']['turns'][0],p['right']['turns'][0]
        equal=l['attribution']['scores']==r['attribution']['scores'];assert equal
        pair_rows.append({'pair':name,'growth':growth,'identical_scores_and_ranking':equal,'identical_admission_trace':l['attribution']['admissions']==r['attribution']['admissions']})
    audit={'scope':'Constructed representation diagnostic, fixture judgment, no model-quality or execution result.','variants':16,'pair_conditions':pair_rows,'rows':rows,'provider_calls':0,'policy_unchanged':True}
    (OUT/'AUDIT.json').write_text(json.dumps(audit,indent=2))
    lines=['# Requested/excluded action role pairs','',audit['scope'],'',f'{len(pair_rows)}/8 paired conditions have identical initial scores and rankings. Full requests are distinct and reach judgment.','',
      '| Case | Growth | Desired rank | First stage | First complete | Requests | Bytes |','|---|---:|---:|---|---:|---:|---:|']
    for r in rows:lines.append(f"| {r['id']} | {r['growth']} | {r['desired']['lexical_rank']} | {r['desired']['stage']} | {r['first_complete']} | {r['requests']} | {r['bytes']} |")
    text='\n'.join(lines)+'\n';(OUT/'REPORT.md').write_text(text)
    (OUT/'REPORT.html').write_text('<!doctype html><meta charset="utf-8"><title>Action role pairs</title><style>body{max-width:1200px;margin:40px auto;font:16px system-ui}pre{white-space:pre-wrap;line-height:1.6}</style><pre>'+html.escape(text)+'</pre>')
    hashes={str(p.relative_to(OUT)):sha(p) for p in OUT.rglob('*') if p.is_file() and p!=OUT/'HASHES.json'}
    (OUT/'HASHES.json').write_text(json.dumps(hashes,indent=2));assert all(sha(OUT/n)==v for n,v in hashes.items())
    print(json.dumps({'pairs':pair_rows,'rows':[{k:r[k] for k in ['id','growth','first_complete','requests']} for r in rows]}))
if __name__=='__main__':main()
