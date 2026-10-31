"""Freeze, run and grade the native progress benchmark. No network calls."""
import hashlib,json,os,subprocess,sys
from pathlib import Path
HERE=Path(__file__).resolve().parent
ROOT=HERE.parents[2]
def sha(p):return hashlib.sha256(p.read_bytes()).hexdigest()
def verify():
    lock=json.loads((HERE/'LOCK.json').read_text())
    for name,h in lock.items():assert sha(ROOT/name)==h,name
    return lock
def summarize(p):
    d=json.loads(p.read_text());rows=[]
    for c in d['cases']:
        turns=c['turns'];last=turns[-1]
        rows.append({'id':c['id'],'positive':c['positive'],'first_complete':next((t['search'] for t in turns if t['complete']),None),'final_status':last['status'],'requests':sum(t['receipt']['requests'] for t in turns),'bytes':sum(t['receipt']['input_bytes'] for t in turns),'repeated_rejections':sum(t['repeated_rejections'] for t in turns),'zero_gain':sum(not t['newly_retained'] for t in turns)})
    failures=[c['id'] for c in rows if c['positive'] and c['first_complete'] is None]
    failures += ['exposure:'+c['id'] for c in d['exposure'] if not c['relation_visible']]
    return {'cases':rows,'exposure':d['exposure'],'quality_failures':failures,'provider_calls':0,'provider_usage':None,'development_only':True}
def main():
    cmd=sys.argv[1]
    if cmd=='freeze':
        assert not (HERE/'LOCK.json').exists()
        paths=[HERE/n for n in ['PROTOCOL.md','cases.json','generate.py','run.py']]
        paths+=list((ROOT/'fixtures/schemas/discovery_progress_v3').glob('*.yaml'))
        (HERE/'LOCK.json').write_text(json.dumps({str(p.relative_to(ROOT)):sha(p) for p in paths},indent=2)+'\n');print('Frozen before native measurement');return
    lock=verify()
    if cmd=='verify':print('Fixture hashes verified');return
    if cmd=='quality':
        s=summarize(Path(sys.argv[2]));print(json.dumps(s,indent=2));sys.exit(bool(s['quality_failures']))
    assert cmd=='run'
    out=Path(sys.argv[2]).resolve();out.mkdir(parents=True,exist_ok=False)
    paths=list((ROOT/'crates/plasm-agent-core/src/discovery_matcher').rglob('*.rs'))
    paths += [ROOT/'crates/plasm-core/src'/n for n in ['capability_exposure.rs','symbol_tuning/expose_surface.rs','domain_lexicon.rs']]
    hashes={str(p.relative_to(ROOT)):sha(p) for p in paths}
    (out/'MANIFEST.json').write_text(json.dumps({'fixtures':lock,'sources':hashes,'head':subprocess.check_output(['git','rev-parse','HEAD'],cwd=ROOT,text=True).strip(),'dirty':subprocess.check_output(['git','diff','--stat'],cwd=ROOT,text=True)},indent=2))
    env=dict(os.environ,PLASM_PROGRESS_REPORT=str(out/'RESULTS.json'))
    with (out/'TEST.log').open('w') as f:
        r=subprocess.run(['cargo','test','-p','plasm-agent-core','--lib','fixed_native_progress_suite_v3','--','--ignored','--nocapture'],cwd=ROOT,env=env,stdout=f,stderr=subprocess.STDOUT)
    verify();assert all(sha(ROOT/n)==h for n,h in hashes.items()),'Source drift'
    if r.returncode:raise SystemExit('Native harness fault; inspect TEST.log')
    s=summarize(out/'RESULTS.json');(out/'SUMMARY.json').write_text(json.dumps(s,indent=2)+'\n');print(json.dumps({'directory':str(out),'quality_failures':s['quality_failures']},indent=2))
if __name__=='__main__':main()
