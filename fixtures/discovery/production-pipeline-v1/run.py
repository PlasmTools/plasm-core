"""One frozen live discovery run. No agent, task execution, or outcome-based tuning."""
import hashlib
import html
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import time
import uuid

ROOT = Path(__file__).resolve().parents[3]
TEST = 'discovery_matcher::online::trajectory_suite::production_pipeline::frozen_production_discovery_pipeline'

def sha(p):
    return hashlib.sha256(p.read_bytes()).hexdigest()

def save(p, data):
    p.write_text(json.dumps(data, indent=2) + '\n')

def fixtures():
    locked = json.loads((ROOT/'fixtures/discovery/trajectory-v4/LOCK.json').read_text())
    original = json.loads((ROOT/'fixtures/discovery/action-role-pairs-v1/LOCK.json').read_text())
    locked.update({p:h for p,h in original.items() if p.startswith('fixtures/discovery/action-role-pairs-v1/') or p.startswith('fixtures/schemas/discovery_trajectory_v4/')})
    assert all(sha(ROOT/p)==h for p,h in locked.items()), 'Frozen fixture drift'
    return locked

def audit(out):
    rows=[]
    trajectory=json.loads((ROOT/'fixtures/discovery/trajectory-v4/cases.json').read_text())
    roles=json.loads((ROOT/'fixtures/discovery/action-role-pairs-v1/cases.json').read_text())
    for p in sorted(out.glob('*/turns.json')):
        turns=json.loads(p.read_text())
        name=p.parent.name
        if name.startswith('trajectory-'):
            case_id=name[len('trajectory-'):].rsplit('-',2)[0]
            case=next(c for c in trajectory['cases'] if c['id']==case_id)
        else:
            profile,side,_=name[len('roles-'):].rsplit('-',2)
            case=next(c for c in roles['cases'] if c['id']==profile+'/'+side)
        first=next((t['search'] for t in turns if all(t['outcomes'])),None)
        prefix=turns[:first] if first else turns
        rows.append(dict(id=p.parent.name, turns=len(turns), first_complete=first,
            gate_pass=first is not None and first<=4,
            no_gain=sum(not t['newly_exposed'] for t in prefix),
            admitted_attempts=sum(t['search_receipt']['requests'] for t in turns),
            admitted_bytes=sum(t['search_receipt']['input_bytes'] for t in turns),
            extra_retained=sum(r['catalog']!='workshop' or r['capability'] not in case['labels'] for r in turns[-1]['exposed']),
            stages_at_gate=turns[min(3,len(turns)-1)]['status']))
    responses=[]; lifecycle=[]; transport=[]
    for p in (out/'receipts').glob('*.json'):
        r=json.loads(p.read_text())
        lane=r.get('lane','discovery_judgment' if r.get('contract') else 'unassigned')
        r['lane']=lane
        if r.get('kind')=='response':
            try: raw=json.loads(r['raw_response'])
            except (ValueError,KeyError): raw={}
            responses.append(dict(lane=lane,attempt_id=r.get('attempt_id'),status=r.get('http_status'),usage=raw.get('usage')))
        elif r.get('kind')=='lifecycle': lifecycle.append(r)
        elif r.get('kind')=='transport': transport.append(r)
    timings=sorted(t['whole_ms'] for p in out.glob('*/turns.json') for t in json.loads(p.read_text()))
    def percentile(q): return timings[min(len(timings)-1,round((len(timings)-1)*q))] if timings else None
    usage={}
    for r in responses:
        lane=usage.setdefault(r['lane'],dict(responses=0,with_usage=0,reported_totals={}))
        lane['responses']+=1
        if isinstance(r['usage'],dict):
            lane['with_usage']+=1
            for k,v in r['usage'].items():
                if isinstance(v,(int,float)) and not isinstance(v,bool): lane['reported_totals'][k]=lane['reported_totals'].get(k,0)+v
    manifest=json.loads((out/'MANIFEST.json').read_text())
    drift=[p for p,h in manifest['sources'].items() if sha(ROOT/p)!=h]
    expected=28 if manifest.get('continuation') else 34
    data=dict(development_only=True, expected_variants=expected, finished_variants=sum(r['turns']==24 for r in rows),
        gate_passes=sum(r['gate_pass'] for r in rows), rows=rows, source_drift=drift,
        response_receipts=responses,transport_faults=transport,
        usage_by_lane=usage,
        lifecycle_receipts=len(lifecycle),
        whole_latency_ms=dict(median=percentile(.5),p90=percentile(.9),p95=percentile(.95),maximum=max(timings,default=None)),
        limitations=['Frozen development cases, not holdout or agent execution.',
          'Old component results used lexical recall and oracle judgment; differences are not causal attribution.',
          'Usage is raw provider-reported usage per response, with missing values unavailable.',
          'Started/dispatch events are not proof of transmission; logical requests are not added to attempt receipts.'])
    save(out/'AUDIT.json',data)
    lines=['# Fixed discovery: production pipeline','',f"Completed variants: {data['finished_variants']}/{expected}. Four-search availability passes among observed variants: {data['gate_passes']}/{len(rows)}.",'',
      '| Variant | Searches | First complete | Gate | No-gain prefix | Admitted attempts |', '|---|---:|---:|---|---:|---:|']
    lines += [f"| {r['id']} | {r['turns']} | {r['first_complete']} | {'PASS' if r['gate_pass'] else 'FAIL'} | {r['no_gain']} | {r['admitted_attempts']} |" for r in rows]
    lines += ['',*data['limitations'],'', 'Detailed failure stages, usage receipts and latency: AUDIT.json. Raw per-turn retrieval and routing receipts are preserved.']
    if (out/'ABORT.json').exists(): lines += ['', '**ABORTED: partial results only.**', (out/'ABORT.json').read_text()]
    (out/'REPORT.md').write_text('\n'.join(lines)+'\n')
    table='<table><tr><th>Variant</th><th>Searches</th><th>First complete</th><th>Gate</th><th>No-gain prefix</th></tr>'+''.join(f"<tr><td>{html.escape(r['id'])}</td><td>{r['turns']}</td><td>{r['first_complete']}</td><td>{'PASS' if r['gate_pass'] else 'FAIL'}</td><td>{r['no_gain']}</td></tr>" for r in rows)+'</table>'
    (out/'REPORT.html').write_text('<!doctype html><meta charset="utf-8"><title>Fixed discovery production pipeline</title><style>body{font:16px system-ui;max-width:1200px;margin:40px auto;background:#101820;color:#e8ecf0}td,th{padding:8px;border-bottom:1px solid #53616b;text-align:left}a{color:#89caff}</style><h1>Fixed discovery: production pipeline</h1>'+f"<p>Completed: {data['finished_variants']}/34; four-search passes among observed variants: {data['gate_passes']}/{len(rows)}.</p>"+table+'<p>Development data; no agent execution. <a href="AUDIT.json">Audit and failure stages</a> · <a href="REPORT.md">Full report</a></p>')
    save(out/'HASHES.json',{str(p.relative_to(out)):sha(p) for p in out.rglob('*') if p.is_file() and p.name not in ['HASHES.json','private.env'] and 'embedding-cache' not in p.parts})
    assert not drift, 'Sources changed during run'

def main():
    if sys.argv[1]=='audit': return audit(Path(sys.argv[2]).resolve())
    out=Path(sys.argv[1]).resolve();out.relative_to(ROOT);out.mkdir(parents=True,exist_ok=False)
    locked=fixtures()
    sources={str(p.relative_to(ROOT)):sha(p) for crate in ['plasm-agent-core','plasm-core','plasm-runtime','plasm-catalog','plasm-compile'] for p in (ROOT/'crates'/crate).rglob('*.rs')}
    sources.update({str(p.relative_to(ROOT)):sha(p) for p in Path(__file__).parent.iterdir() if p.is_file()})
    sources.update({str(p.relative_to(ROOT)):sha(p) for p in (ROOT/'fixtures/schemas/discovery_pipeline_v1').glob('*.yaml')})
    prior=Path(sys.argv[2]).resolve() if len(sys.argv)>2 else None
    if prior:
        previous=json.loads((prior/'AUDIT.json').read_text())
        assert len(previous['rows'])==6 and all(r['turns']==24 and r['id'].endswith(('-0-false','-0-true')) for r in previous['rows'])
    save(out/'MANIFEST.json',dict(fixtures=locked,sources=sources,continuation=str(prior) if prior else None,head=subprocess.check_output(['git','rev-parse','HEAD'],cwd=ROOT,text=True).strip()))
    (out/'PREREGISTRATION.md').write_text((Path(__file__).parent/'PROTOCOL.md').read_text())
    name='plasm-fixed-production-'+uuid.uuid4().hex[:10]
    save(out/'OWNER.json',dict(pid=os.getpid(),database=name))
    docker=lambda *args: subprocess.check_output(['docker',*args],text=True).strip()
    private=out/'private.env'
    launched=False
    try:
        with (out/'PREFLIGHT.log').open('x') as log:
            subprocess.run(['bash','build/bazel/linux.sh','test','//crates/plasm-agent-core:unit_tests','--test_arg=production_pipeline_catalog_preflight','--test_output=all','--nocache_test_results'],cwd=ROOT,stdout=log,stderr=subprocess.STDOUT,check=True)
        assert '1 passed; 0 failed' in (out/'PREFLIGHT.log').read_text()
        docker('run','-d','--name',name,'-e','POSTGRES_PASSWORD=abstract-proof','-p','127.0.0.1::5432','pgvector/pgvector:pg16')
        for _ in range(60):
            if subprocess.run(['docker','exec',name,'pg_isready','-U','postgres'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL).returncode==0: break
            time.sleep(1)
        else: raise RuntimeError('Owned proof database unavailable')
        port=docker('port',name,'5432/tcp').rsplit(':',1)[1]
        key=os.environ.get('OPENROUTER_API_KEY')
        if not key:
            for line in (ROOT.parent/'.env').read_text().splitlines():
                assignment=line.strip().removeprefix('export ')
                if assignment.startswith('OPENROUTER_API_KEY='):
                    values=shlex.split(assignment.split('=',1)[1],comments=True)
                    key=values[0] if values else None
        assert key and '\n' not in key
        fd=os.open(private,os.O_CREAT|os.O_EXCL|os.O_WRONLY,0o600)
        with os.fdopen(fd,'w') as f:f.write('OPENROUTER_API_KEY='+key+'\n')
        (out/'receipts').mkdir();(out/'embedding-cache').mkdir()
        cache_sources = [prior] if prior else []
        cache_sources += [Path(p).resolve() for p in os.environ.get('PLASM_PIPELINE_REUSE_CACHES', '').split(os.pathsep) if p]
        reused = {}
        for cache_source in cache_sources:
            for p in (cache_source/'embedding-cache').glob('*.json'):
                dest = out/'embedding-cache'/p.name
                assert not dest.exists() or sha(dest) == sha(p), 'Conflicting cached vectors'
                shutil.copyfile(p, dest)
                reused[str(p)] = sha(p)
        save(out/'REUSED-EMBEDDINGS.json', reused)
        rel='/workspace/'+str(out.relative_to(ROOT))
        checkout=hashlib.sha256(str(ROOT).encode()).hexdigest()
        arch=docker('info','--format','{{.Architecture}}');platform='linux/arm64' if arch in ['aarch64','arm64'] else 'linux/amd64'
        cmd=['docker','run','--rm','--name',name+'-runner','--platform',platform,'--env-file',str(private),
          '--mount',f'type=bind,source={ROOT},target=/workspace','--mount','type=volume,source=plasm-bazel-linux-tools-v1,target=/tools',
          '--mount',f'type=volume,source=plasm-bazel-linux-cache-v2-{os.getuid()},target=/cache','--workdir','/workspace',
          '-e',f'PLASM_BAZEL_OUTPUT_BASE=/cache/outputs/{checkout}-{arch}','-e','PLASM_BAZEL_CORE=/workspace',
          '-e',f'PLASM_BUILD_UID={os.getuid()}','-e',f'PLASM_BUILD_GID={os.getgid()}',
          'rust:1.99-trixie@sha256:6ff07edce8775d0f64be7aba9197229407301bddf2054d62c27b541a6238a181',
          'bash','/workspace/build/bazel/linux-entrypoint.sh','test','//crates/plasm-agent-core:unit_tests',
          '--test_arg='+TEST,'--test_arg=--exact','--test_arg=--ignored','--test_arg=--nocapture',
          '--test_output=all','--nocache_test_results','--test_timeout=28800','--test_env=OPENROUTER_API_KEY',
          '--test_env=PLASM_DISCOVERY_CAPABILITY_MATCH_MODEL=openai/gpt-6-luna-decisions',
          '--test_env=PLASM_TEST_POSTGRES_URL=postgresql://postgres:abstract-proof@host.docker.internal:'+port+'/postgres',
          '--test_env=PLASM_PIPELINE_REPORT='+rel,'--test_env=PLASM_PIPELINE_EMBEDDING_CACHE='+rel+'/embedding-cache',
          '--test_env=PLASM_DISCOVERY_REJECTION_DIR='+rel+'/receipts','--test_env=PLASM_EMBEDDING_RECORD_DIR='+rel+'/receipts']
        if prior: cmd.append('--test_env=PLASM_PIPELINE_SKIP_ZERO_TRAJECTORY=1')
        fixtures();assert all(sha(ROOT/p)==h for p,h in sources.items())
        with (out/'LAUNCHED').open('x') as f:f.write(time.strftime('%Y-%m-%dT%H:%M:%SZ',time.gmtime()))
        launched=True
        with (out/'RUN.log').open('x') as log:
            result=subprocess.run(cmd,cwd=ROOT,stdout=log,stderr=subprocess.STDOUT)
        save(out/'EXIT.json',{'code':result.returncode})
        if result.returncode: raise RuntimeError('Native pipeline fault: inspect RUN.log; no automatic retry')
    except Exception as e:
        save(out/'ABORT.json',dict(error=str(e),launched=launched))
        raise
    finally:
        private.unlink(missing_ok=True)
        if name in docker('ps','-a','--format','{{.Names}}').splitlines(): docker('rm','-fv',name)
        save(out/'CLEANUP.json',dict(database_removed=name,credentials_removed=not private.exists()))
        audit(out)

if __name__=='__main__': main()
