"""Frozen strict signal judge and registered native-observation lookup."""
import collections
import hashlib
import json
import pathlib
import os

root = pathlib.Path(os.environ['PLASM_EXPERIMENT_OUTPUT']).resolve()
core = pathlib.Path(__file__).resolve().parents[3]
fixture = core/'fixtures/discovery/caller-revision-v1'
labels = json.loads((fixture/'labels.json').read_text())
gold = {case: signal for signal, ids in labels.items() for case in ids}
lock = json.loads((root/'LOCK.json').read_text())
assert all(hashlib.sha256(pathlib.Path(p).read_bytes()).hexdigest()==h for p,h in lock.items())
native_path = pathlib.Path(os.environ['PLASM_NATIVE_SUMMARY']).resolve()
assert str(native_path) in lock, 'Native summary must be a frozen input'
native = json.loads(native_path.read_text())['observations']
rows=[]
tokens=collections.Counter()
usage_available=0
for case in json.loads((fixture/'cases.json').read_text()):
    case_id=case['id']; file=root/(case_id+'.json')
    if not file.exists():
        rows.append(dict(id=case_id,status='unstarted_or_interrupted',expected=gold[case_id],projections=[]));continue
    response=json.loads(file.read_text())
    try:
        answer=json.loads(response['text'])
        valid=isinstance(answer,dict) and set(answer)=={'signal','reason'} and answer['signal'] in ('continue','revise') and isinstance(answer['reason'],str) and bool(answer['reason'].strip())
    except (ValueError,KeyError,TypeError):
        valid=False;answer=None
    predicted=answer['signal'] if valid else None
    status='correct' if predicted==gold[case_id] else ('invalid' if not valid else 'incorrect')
    projections=[]
    if valid:
        stable=gold[case_id]=='continue'
        scenario=['ExpensiveFirst','ExpensiveTarget','Equal','Connected'][int(case_id[-2:])-1] if stable else 'Changed'
        arm=('ExplicitCorrect' if predicted=='continue' else 'OneSpuriousRevision') if stable else ('ExplicitCorrect' if predicted=='revise' else 'MissedRevision')
        projections=[r for r in native if r['case']==scenario and r['arm']==arm]
        assert len(projections)==4
    usage=response.get('usage')
    if isinstance(usage,dict):
        usage_available+=1
        for k in ['inputTokens','outputTokens','totalTokens']:
            if isinstance(usage.get(k),(int,float)):tokens[k]+=usage[k]
    rows.append(dict(id=case_id,expected=gold[case_id],predicted=predicted,status=status,
        reason=answer.get('reason') if valid else None,elapsed_ms=response['elapsedMs'],
        finish_reason=response['finishReason'],usage=usage,projections=projections))
wire=[]
for file in (root/'wire').glob('*.jsonl'):
    wire.extend(json.loads(line) for line in file.read_text().splitlines())
projections=[p for r in rows for p in r['projections']]
print(json.dumps(dict(exploratory=True,counts=dict(collections.Counter(r['status'] for r in rows)),
    false_revisions=sum(r.get('predicted')=='revise' and r['expected']=='continue' for r in rows),
    missed_revisions=sum(r.get('predicted')=='continue' and r['expected']=='revise' for r in rows),
    projected_observations=len(projections),projected_reached=sum(p['first'] is not None for p in projections),
    projected_unresolved=4*(24-sum(bool(r['projections']) for r in rows)),
    usage_available=usage_available,sdk_token_totals=dict(tokens),cost=None,
    cost_note='Unavailable unless explicitly recorded in provider metadata; no price inference.',
    actual_wire_requests=sum(e.get('kind')=='request' for e in wire),
    wire_statuses=dict(collections.Counter(str(e.get('status')) for e in wire if e.get('kind')=='response')),
    source_hashes_verified=len(lock),rows=rows),indent=2))
