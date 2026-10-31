"""Frozen availability and signal judge; request fidelity is a separate manual audit."""
import collections
import hashlib
import json
import pathlib
import os

root=pathlib.Path(os.environ['PLASM_EXPERIMENT_OUTPUT']).resolve()
lock=json.loads((root/'LOCK.json').read_text())
assert all(hashlib.sha256(pathlib.Path(p).read_bytes()).hexdigest()==h for p,h in lock.items())
controls={r['id']:r for r in json.loads((root/'READY.json').read_text())['controls']}
rows=[]
for case in json.loads((root/'cases.json').read_text()):
    p=root/(case['id']+'.json')
    row=json.loads(p.read_text()) if p.exists() else dict(id=case['id'],status='unstarted',turns=[])
    row['oracle_first']=controls[case['id']]['first']
    row['first']=next((t['turn'] for t in row['turns'] if t.get('native',{}).get('found')),None)
    row['signal_errors']=[dict(turn=t['turn'],actual=t['signal'],expected=t['expected_signal']) for t in row['turns'] if 'signal' in t and t['signal']!=t['expected_signal']]
    rows.append(row)
models=[json.loads(p.read_text()) for p in root.glob('*.model.json')]
tokens=collections.Counter()
for model in models:
    for k in ['inputTokens','outputTokens','totalTokens']:
        value=model.get('usage',{}).get(k)
        if isinstance(value,(int,float)):tokens[k]+=value
wire=[json.loads(l) for p in (root/'wire').glob('*.jsonl') for l in p.read_text().splitlines()]
print(json.dumps(dict(exploratory=True,statuses=dict(collections.Counter(r['status'] for r in rows)),
    signal_errors=sum(len(r['signal_errors']) for r in rows),loss_against_oracle=sum(r['first'] is None or r['first']>r['oracle_first'] for r in rows),
    logical_responses=len(models),wire_requests=sum(e.get('kind')=='request' for e in wire),wire_statuses=dict(collections.Counter(str(e.get('status')) for e in wire if e.get('kind')=='response')),
    sdk_token_totals=dict(tokens),source_hashes_verified=len(lock),rows=rows),indent=2))
