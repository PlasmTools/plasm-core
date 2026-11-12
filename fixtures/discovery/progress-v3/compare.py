"""Compare two frozen-suite receipts without a weighted headline score."""
import json,sys
from pathlib import Path
def load(path):
    p=Path(path); d=json.loads((p/'RESULTS.json').read_text());m=json.loads((p/'MANIFEST.json').read_text())
    return d,m
def metrics(c):
    ts=c['turns']
    return dict(first_complete=next((t['search'] for t in ts if t['complete']),None),
                first_relation=next((t['search'] for t in ts if t['relation_visible']),None),
                first_judgments={x['name']:next(({'search':t['search'],'choice':s['judgment']} for t in ts for s in t['status'] if s['name']==x['name'] and s['judgment'] is not None),None) for x in ts[0]['status']},
                requests=sum(t['receipt']['requests'] for t in ts),
                bytes=sum(t['receipt']['input_bytes'] for t in ts),
                repeated_rejections=sum(t['repeated_rejections'] for t in ts),
                zero_gain=sum(not t['newly_retained'] for t in ts))
before,bm=load(sys.argv[1]);after,am=load(sys.argv[2])
assert bm['fixtures']==am['fixtures'],'Different fixture versions cannot be paired silently'
old={c['id']:c for c in before['cases']};new={c['id']:c for c in after['cases']}
assert old.keys()==new.keys()
rows=[]
for name in old:
    a=metrics(old[name]);b=metrics(new[name]);limit=len(old[name]['turns'])+1
    first=lambda m:m['first_complete'] if m['first_complete'] is not None else limit
    rows.append({'id':name,'before':a,'after':b,'coverage_or_speed':'improved' if first(b)<first(a) else 'regressed' if first(b)>first(a) else 'unchanged'})
eo={x['id']:x['relation_visible'] for x in before['exposure']};en={x['id']:x['relation_visible'] for x in after['exposure']}
assert eo.keys()==en.keys()
print(json.dumps({'development_only':True,'cases':rows,'exposure':{k:{'before':eo[k],'after':en[k]} for k in eo},
                  'regressions':[r['id'] for r in rows if r['coverage_or_speed']=='regressed'],
                  'note':'No composite score. Inspect bytes/requests and every outcome; a development improvement is not holdout acceptance.'},indent=2))
