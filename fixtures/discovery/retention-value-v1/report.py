"""Operational comparisons, preserving old labels and quality conditions."""
import collections
import html
import json
from run import HERE, OUT, ROOT, sha, verify

BASELINES = {
    'trajectory-v4':'trajectory-v4-harness-parity-20261030',
    'resource-breadth-v5':'resource-breadth-v5-baseline-r2',
    'root-diversity-v1':'root-diversity-v1-baseline',
}


def key(row):
    return (row['id'],row['distractors'],row['rotating'])


def summarize(case):
    turns=case['turns']
    first=next((t['search'] for t in turns if all(t['outcomes'])),None)
    prefix=turns[:first] if first is not None else turns
    old_useful=0; useful_no_gain=0
    for t in prefix:
        useful=sum(s['stage']=='retained' for s in t['status'])
        useful_no_gain+=useful==old_useful
        old_useful=useful
    return {**{k:case[k] for k in ['id','cohort','arm','distractors','rotating']},
            'first_complete':first,'gate_pass':first is not None and first<=4,
            'first_per_outcome':[next((t['search'] for t in turns if t['outcomes'][i]),None) for i in range(len(turns[0]['outcomes']))],
            'requests_to_completion_or_horizon':sum(t['receipt']['requests'] for t in prefix),
            'bytes_to_completion_or_horizon':sum(t['receipt']['input_bytes'] for t in prefix),
            'declaration_bytes':prefix[-1]['declaration_bytes'],
            'incidental_exposure':prefix[-1]['incidental_exposure'],
            'total_exposure':len(prefix[-1]['exposed_references']),
            'original_no_unrelated_exposure':all(t['incidental_exposure']==0 for t in turns),
            'original_quality_pass':first is not None and first<=4 and all(t['incidental_exposure']==0 for t in turns),
            'missing_outcome_searches':sum(sum(not v for v in t['outcomes']) for t in turns),
            'no_gain_prefix':sum(not t['newly_exposed'] for t in prefix),'useful_no_gain_prefix':useful_no_gain,
            'repeated_rejections_prefix':sum(t['repeated_rejections'] for t in prefix),
            'post_completion_requests':sum(t['receipt']['requests'] for t in turns[len(prefix):]),
            'indexing_us':case['timing']['indexing_us'],
            'prefix_selection_us':sum(case['timing']['selection_us'][:len(prefix)]),
            'prefix_teaching_us':sum(case['timing']['teaching_us'][:len(prefix)]),
            'final_status':turns[-1]['status']}


def main():
    lock=json.loads((OUT/'LOCK.json').read_text());verify(lock['fixtures']);verify(lock['sources'])
    records=[json.loads(l) for l in (OUT/'RESULTS.jsonl').read_text().splitlines()]
    rows=[summarize(r) for r in records]
    complete=len(rows)==116 and json.loads((OUT/'EXIT.json').read_text())['code']==0
    assert len({(r['cohort'],r['arm'],*key(r)) for r in rows})==len(rows)
    historical={}; parity=[]
    for cohort,name in BASELINES.items():
        path=ROOT/'eval-reports'/name/'SUMMARY.json'
        historical[cohort]={key(r):r for r in json.loads(path.read_text())['cases']}
    for row in rows:
        if row['arm']=='fixture_judgment':
            old=historical[row['cohort']][key(row)]
            differences={k:[old[k],row[k]] for k in ['first_complete','requests_to_completion_or_horizon','bytes_to_completion_or_horizon','declaration_bytes'] if old[k]!=row[k]}
            if differences:parity.append({'cohort':row['cohort'],'key':key(row),'differences':differences})
    comparisons=[]
    for cohort in BASELINES:
        b={key(r):r for r in rows if r['cohort']==cohort and r['arm']=='fixture_judgment'}
        c={key(r):r for r in rows if r['cohort']==cohort and r['arm']=='retain_admitted'}
        for k in b.keys() & c.keys():
            baseline,candidate=b[k],c[k]
            comparisons.append({'cohort':cohort,'id':k[0],'distractors':k[1],'rotating':k[2],
              'baseline':baseline,'candidate':candidate,
              'gate_recovery':not baseline['gate_pass'] and candidate['gate_pass'],
              'later_completion':baseline['first_complete'] is not None and (candidate['first_complete'] is None or candidate['first_complete']>baseline['first_complete']),
              'delta_requests':candidate['requests_to_completion_or_horizon']-baseline['requests_to_completion_or_horizon'],
              'delta_bytes':candidate['bytes_to_completion_or_horizon']-baseline['bytes_to_completion_or_horizon'],
              'delta_declarations':candidate['declaration_bytes']-baseline['declaration_bytes']})
    comparisons.sort(key=lambda p:(p['cohort'],p['id'],p['distractors'],p['rotating']))
    groups={}
    for cohort in BASELINES:
        groups[cohort]={}
        for arm in ['fixture_judgment','retain_admitted']:
            rs=[r for r in rows if r['cohort']==cohort and r['arm']==arm]
            groups[cohort][arm]={'variants':len(rs),'available_by_four':sum(r['gate_pass'] for r in rs),
              'original_quality_pass':sum(r['original_quality_pass'] for r in rs),
              **{k:sum(r[k] for r in rs) for k in ['requests_to_completion_or_horizon','bytes_to_completion_or_horizon','declaration_bytes','incidental_exposure','useful_no_gain_prefix','prefix_selection_us','prefix_teaching_us']}}
    dominance=complete and not parity and any(p['gate_recovery'] for p in comparisons) and all(not p['later_completion'] and p['delta_requests']<=0 and p['delta_declarations']<=0 for p in comparisons)
    audit={'complete':complete,'historical_parity_differences':parity,'operational_dominance':dominance,
           'groups':groups,'comparisons':comparisons,'rows':rows,'provider_calls':0,'provider_tokens':None,'provider_cost':None,
           'scope':'Frozen development mechanics with scripted requests; not model quality, actual provider savings, adaptive stopping or execution success.',
           'original_quality':'Useful-path availability <=4 searches AND no unrelated exposure; report separately, do not relabel.',
           'timing':'Indexing, selection and teaching measured locally. Prefix excludes forced post-completion work; single timings are descriptive.'}
    (OUT/'AUDIT.json').write_text(json.dumps(audit,indent=2)+'\n')
    lines=['# Operational retention ablation','',f'Complete: {complete}. Historical baseline discrepancies: {len(parity)}. Operational dominance: {dominance}.','',audit['scope'],'',
      '| Cohort | Arm | Available by search 4 | Original quality | Prefix requests | Prefix admitted bytes | Declaration bytes | Incidental exposures |',
      '|---|---|---:|---:|---:|---:|---:|---:|']
    for cohort,arms in groups.items():
        for arm,g in arms.items():
            lines.append(f"| {cohort} | {arm} | {g['available_by_four']}/{g['variants']} | {g['original_quality_pass']}/{g['variants']} | {g['requests_to_completion_or_horizon']} | {g['bytes_to_completion_or_horizon']} | {g['declaration_bytes']} | {g['incidental_exposure']} |")
    lines+=['','Totals sum variants, including intentional duplicates; they are not a population estimate. No provider tokens/cost are inferred from bytes. Retain-admitted still constructs the same bounded packets, so it does not measure a no-packet implementation.','',audit['timing'],'',
      '## Every paired variant','','| Cohort / case | Distractors | Rotate | Completion before → after | Δ requests | Δ admitted bytes | Δ declaration bytes |','|---|---:|---|---|---:|---:|---:|']
    for p in comparisons:
        lines.append(f"| {p['cohort']} / {p['id']} | {p['distractors']} | {p['rotating']} | {p['baseline']['first_complete']} → {p['candidate']['first_complete']} | {p['delta_requests']} | {p['delta_bytes']} | {p['delta_declarations']} |")
    if parity:lines+=['','STOP: harness parity failed; no design inference.',json.dumps(parity)]
    text='\n'.join(lines)+'\n'
    (OUT/'REPORT.md').write_text(text)
    (OUT/'REPORT.html').write_text('<!doctype html><meta charset="utf-8"><title>Operational discovery gradient</title><style>body{max-width:1400px;margin:40px auto;background:#101820;color:#e7eef2;font:16px system-ui}pre{white-space:pre-wrap;line-height:1.5}</style><pre>'+html.escape(text)+'</pre>')
    hashes={p.name:sha(p) for p in OUT.iterdir() if p.is_file() and p.name!='HASHES.json'}
    (OUT/'HASHES.json').write_text(json.dumps(hashes,indent=2))
    assert all(sha(OUT/n)==h for n,h in hashes.items())
    print(json.dumps({'complete':complete,'parity_differences':parity,'dominance':dominance,'groups':groups}))


if __name__=='__main__':main()
