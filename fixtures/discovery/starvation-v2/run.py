"""Frozen native starvation run, serial cases and bounded local construction."""
import hashlib,json,os,signal,subprocess,sys,time
from pathlib import Path

HERE=Path(__file__).resolve().parent
ROOT=HERE.parents[2]
TEST='discovery_matcher::online::starvation_suite::fixed_native_starvation_suite_v2'
def sha(p):return hashlib.sha256(p.read_bytes()).hexdigest()
def verify():
    lock=json.loads((HERE/'LOCK.json').read_text())
    assert all(sha(ROOT/p)==value for p,value in lock.items()),'Frozen inputs changed'
    cases=json.loads((HERE/'cases.json').read_text())
    assert len(cases)==len({c['id'] for c in cases})==24
    for c in cases:
        for o in c['outcomes']:
            assert o['alternatives']
            assert all(c['labels'][s]=='relevant' for path in o['alternatives'] for s in path)
    return cases

def summary(directory):
    rows=[]
    for case in verify():
        folder=directory/case['id'].replace('/','__')
        status=json.loads((folder/'STATUS.json').read_text())
        row={k:case[k] for k in ['id','topology','profile','count']}
        row.update(status)
        if status['state']=='complete':
            result=json.loads((folder/'RESULTS.json').read_text())['forward']
            row['outcomes']={str(t['search']):{o['id']:o['complete'] for o in t['outcomes']} for t in result['turns']}
            row['requests']=sum(t['receipt']['requests'] for t in result['turns'])
            row['input_bytes']=sum(t['receipt']['input_bytes'] for t in result['turns'])
            row['max_packet_bytes']=max(t['receipt']['input_bytes'] for t in result['turns'])
            row['targets_at_4']=result['turns'][-1]['statuses']
            row['timings']=result['timings']
            assert row['max_packet_bytes']<=16000
        elif (folder/'TRACE.jsonl').exists():
            trace=[json.loads(line) for line in (folder/'TRACE.jsonl').read_text().splitlines() if line.strip()]
            row['last_phase']=trace[-1]['phase'] if trace else None
            row['completed_searches']=sum(t['phase']=='completed_search' for t in trace)
        rows.append(row)
    result={'cases':rows,'provider_calls':0,'provider_usage':None,
            'completed':sum(r['state']=='complete' for r in rows),
            'timeouts':sum(r['state']=='local_timeout' for r in rows),
            'faults':sum(r['state']=='native_fault' for r in rows)}
    (directory/'SUMMARY.json').write_text(json.dumps(result,indent=2)+'\n')
    print(json.dumps({'completed':result['completed'],'timeouts':result['timeouts'],'faults':result['faults'],
                      'rows':[{k:v for k,v in r.items() if k in ['id','state','outcomes','requests','input_bytes','last_phase']} for r in rows]},indent=2))

def main():
    command=sys.argv[1]
    if command=='freeze':
        assert not (HERE/'LOCK.json').exists()
        paths=[HERE/n for n in ['generate.py','cases.json','noise.json','PROTOCOL.md']]
        paths+=sorted((ROOT/'fixtures/schemas/discovery_starvation_v2').rglob('*.yaml'))
        (HERE/'LOCK.json').write_text(json.dumps({str(p.relative_to(ROOT)):sha(p) for p in paths},indent=2)+'\n')
        verify();print('24 starvation cases, labels and schemas frozen before selection.');return
    cases=verify()
    if command=='verify':print('Fixture hashes and alternative-path labels verified.');return
    if command=='summary':summary(Path(sys.argv[2]));return
    assert command=='run'
    raise SystemExit('v2 is archived invalid schema evidence; use starvation-v2.1/run.py')
    destination=Path(sys.argv[2]).resolve();destination.mkdir(parents=True,exist_ok=False)
    sources=list((ROOT/'crates/plasm-agent-core/src/discovery_matcher').rglob('*.rs'))
    sources += [ROOT/'crates/plasm-agent-core/src/discovery_matcher.rs',ROOT/'crates/plasm-core/src/prerequisites.rs',
                ROOT/'crates/plasm-core/src/domain_lexicon.rs',HERE/'run.py']
    manifest={'fixture_lock':json.loads((HERE/'LOCK.json').read_text()),'source_hashes':{str(p.relative_to(ROOT)):sha(p) for p in sources},
              'head':subprocess.check_output(['git','rev-parse','HEAD'],cwd=ROOT,text=True).strip(),'local_case_deadline_seconds':180}
    (destination/'MANIFEST.json').write_text(json.dumps(manifest,indent=2)+'\n')
    with (destination/'BUILD.jsonl').open('w') as log:
        built=subprocess.run(['cargo','test','-p','plasm-agent-core','--lib','--no-run','--message-format=json'],cwd=ROOT,stdout=log,stderr=subprocess.STDOUT)
    if built.returncode:raise SystemExit('Build failed; evidence preserved')
    executable=None
    for line in (destination/'BUILD.jsonl').read_text().splitlines():
        try:r=json.loads(line)
        except ValueError:continue
        if r.get('reason')=='compiler-artifact' and r.get('executable') and r['target']['name']=='plasm_agent_core':executable=r['executable']
    assert executable
    (destination/'BINARY.json').write_text(json.dumps({'path':executable,'sha256':sha(Path(executable))},indent=2)+'\n')
    for case in cases:
        folder=destination/case['id'].replace('/','__');folder.mkdir()
        env=dict(os.environ,CARGO_MANIFEST_DIR=str(ROOT/'crates/plasm-agent-core'),PLASM_STARVATION_CASE=case['id'],PLASM_STARVATION_REPORT=str(folder/'RESULTS.json'),PLASM_STARVATION_TRACE=str(folder/'TRACE.jsonl'))
        start=time.monotonic()
        with (folder/'TEST.log').open('w') as log:
            proc=subprocess.Popen([executable,'--exact',TEST,'--ignored','--nocapture'],cwd=ROOT,env=env,stdout=log,stderr=subprocess.STDOUT,start_new_session=True)
            try:
                code=proc.wait(timeout=180);state='complete' if code==0 else 'native_fault'
            except subprocess.TimeoutExpired:
                os.killpg(proc.pid,signal.SIGTERM)
                try:proc.wait(timeout=5)
                except subprocess.TimeoutExpired:os.killpg(proc.pid,signal.SIGKILL);proc.wait()
                state='local_timeout';code=proc.returncode
        (folder/'STATUS.json').write_text(json.dumps({'state':state,'exit_code':code,'wall_seconds':time.monotonic()-start},indent=2)+'\n')
        print(case['id'],state,flush=True)
    verify()
    assert all(sha(ROOT/name)==value for name,value in manifest['source_hashes'].items()),'Source changed during run'
    summary(destination)
    (destination/'SHA256.json').write_text(json.dumps({str(p.relative_to(destination)):sha(p) for p in destination.rglob('*') if p.is_file()},indent=2)+'\n')
    if json.loads((destination/'SUMMARY.json').read_text())['faults']:
        raise SystemExit('Native faults recorded; suite incomplete')

if __name__=='__main__':main()
