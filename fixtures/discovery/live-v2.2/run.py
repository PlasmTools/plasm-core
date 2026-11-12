import hashlib,json,os,shlex,subprocess,sys
from pathlib import Path
HERE=Path(__file__).resolve().parent
ROOT=HERE.parents[2]
OUT=ROOT.parent/'target/discovery-live-native-v2_2-certified-20261029'
TEST='discovery_matcher::online::live_gate::native_live_discovery_gate_v2'
def sha(p):return hashlib.sha256(p.read_bytes()).hexdigest()
def save(p,v):p.write_text(json.dumps(v,indent=2)+'\n')
def frozen():return [HERE/x for x in ['cases.json','PROTOCOL.md','LABELS.md','generate.py','repair.py','ERRATUM.md']]+sorted((ROOT/'fixtures/schemas/discovery_live_v2_2').rglob('*.yaml'))
def verify():
    for p,h in json.loads((HERE/'LOCK.json').read_text()).items():assert sha(ROOT/p)==h,p
def sources():
    paths=sorted((ROOT/'crates/plasm-agent-core/src/discovery_matcher').rglob('*.rs'))+[ROOT/'crates/plasm-agent-core/src/discovery_matcher.rs',ROOT/'crates/plasm-agent-core/src/decision_transport.rs',ROOT/'crates/plasm-agent-core/src/decision_codec.rs',HERE/'run.py']
    return {str(p.relative_to(ROOT)):sha(p) for p in paths}
def check_sources():
    for p,h in json.loads((OUT/'SOURCES.json').read_text()).items():assert sha(ROOT/p)==h,p
def execute(mode):
    verify();check_sources()
    binary=json.loads((OUT/'BINARY.json').read_text());assert sha(Path(binary['path']))==binary['sha256']
    env=dict(os.environ,CARGO_MANIFEST_DIR=str(ROOT/'crates/plasm-agent-core'),PLASM_LIVE_GATE_MODE=mode,PLASM_LIVE_GATE_TRACE=str(OUT/(mode+'.jsonl')))
    if mode=='live':
        assert json.loads((OUT/'ORACLE-AUDIT.json').read_text())['passed']
        if not env.get('OPENROUTER_API_KEY'):
            for line in (ROOT.parent/'.env').read_text().splitlines():
                if line.strip().startswith('OPENROUTER_API_KEY='):
                    value=shlex.split(line.split('=',1)[1],comments=True);assert len(value)==1
                    env['OPENROUTER_API_KEY']=value[0]
        assert env.get('OPENROUTER_API_KEY')
        with (OUT/'LAUNCHED').open('x') as f:f.write('native live gate v2\n')
    with (OUT/(mode+'.log')).open('w') as log:
        result=subprocess.run([binary['path'],'--exact',TEST,'--ignored','--nocapture'],cwd=ROOT,env=env,stdout=log,stderr=subprocess.STDOUT,timeout=7200)
    save(OUT/(mode+'-EXIT.json'),{'exit_code':result.returncode});check_sources();verify()
    assert result.returncode==0,'Native gate fault; inspect preserved trace'
    print(mode,'complete')
def main():
    command=sys.argv[1]
    if command=='freeze':
        assert not (HERE/'LOCK.json').exists();save(HERE/'LOCK.json',{str(p.relative_to(ROOT)):sha(p) for p in frozen()});print('Frozen fixture files',len(frozen()))
    elif command=='build':
        verify();OUT.mkdir(exist_ok=False);save(OUT/'SOURCES.json',sources())
        with (OUT/'BUILD.jsonl').open('w') as log:result=subprocess.run(['cargo','test','-p','plasm-agent-core','--lib','--no-run','--message-format=json'],cwd=ROOT,stdout=log,stderr=subprocess.STDOUT)
        assert result.returncode==0,'Build failed'
        for line in (OUT/'BUILD.jsonl').read_text().splitlines():
            try:r=json.loads(line)
            except ValueError:continue
            if r.get('reason')=='compiler-artifact' and r.get('executable') and r['target']['name']=='plasm_agent_core':binary=r['executable']
        save(OUT/'BINARY.json',dict(path=binary,sha256=sha(Path(binary))));check_sources();print('Built')
    elif command in ['oracle','live']:execute(command)
    elif command=='verify':verify();print('Fixtures verified')
if __name__=='__main__':main()
