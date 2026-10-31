import fs from 'node:fs';
import path from 'node:path';
import {fileURLToPath, pathToFileURL} from 'node:url';
import {createRequire} from 'node:module';
import {createHash} from 'node:crypto';

const here=path.dirname(fileURLToPath(import.meta.url));
const core=path.resolve(here,'../../..');
if(!process.env.PLASM_EXPERIMENT_OUTPUT || !process.env.PLASM_NATIVE_SUMMARY)throw Error('Set PLASM_EXPERIMENT_OUTPUT and PLASM_NATIVE_SUMMARY');
const out=path.resolve(process.env.PLASM_EXPERIMENT_OUTPUT);
if(process.argv.includes('--freeze'))fs.mkdirSync(out,{recursive:true});
const root = path.dirname(core);
const fixtures = path.join(core, 'fixtures/discovery/caller-revision-v1');
const agent = path.join(root, 'scripts/appworld/agent');
const read = p => fs.readFileSync(p, 'utf8');
const sha = p => createHash('sha256').update(fs.readFileSync(p)).digest('hex');
const save = (p,x) => fs.writeFileSync(path.join(out,p),JSON.stringify(x,null,2)+'\n',{mode:0o600});
const sources = ['PROMPT.txt','cases.json','labels.json','PREREGISTRATION.md'].map(p=>path.join(fixtures,p));
sources.push(fileURLToPath(import.meta.url), path.join(here,'audit.py'), ...['resolve-eval-model.ts','provider-throttle.ts','model-wire-trace.ts'].map(p=>path.join(agent,'src',p)),path.join(agent,'package.json'),path.join(agent,'package-lock.json'),path.resolve(process.env.PLASM_NATIVE_SUMMARY));
if (process.argv.includes('--freeze')) {
  if(fs.existsSync(path.join(out,'LOCK.json'))) throw new Error('Already frozen');
  save('LOCK.json',Object.fromEntries(sources.map(p=>[p,sha(p)])));
  console.log('Frozen inputs, caller sources and native replay lookup.');
  process.exit(0);
}
const lock = JSON.parse(read(path.join(out,'LOCK.json')));
for(const p of sources) if(lock[p]!==sha(p)) throw new Error('Frozen source drift: '+p);
const cases = JSON.parse(read(path.join(fixtures,'cases.json')));
const system = read(path.join(fixtures,'PROMPT.txt'));
if(cases.length!==24 || new Set(cases.map(x=>x.id)).size!==24) throw new Error('Case inventory');
const payload = c => JSON.stringify({previous_request:c.previous_request,events:c.events,current_request:c.current_request});
for(const c of cases) if(Buffer.byteLength(system+payload(c))+1000>16000) throw new Error('Input budget');
const require = createRequire(path.join(agent,'package.json'));
const {generateText} = await import(pathToFileURL(require.resolve('ai')).href);
const {resolveEvalModel} = await import(pathToFileURL(path.join(agent,'src/resolve-eval-model.ts')).href);
if(process.argv.includes('--preflight')) {
  console.log('PASS: 24 inputs, frozen hashes, byte bounds, existing caller imports; no provider calls.');
  process.exit(0);
}
if(!process.argv.includes('--run')) throw new Error('Specify --preflight or --run');
if(!process.env.OPENROUTER_API_KEY) {
  const line = read(path.join(root,'.env')).split('\n').map(x=>x.trim().replace(/^export\s+/,'')) .find(x=>x.startsWith('OPENROUTER_API_KEY='));
  if(!line) throw new Error('Missing configured provider credential');
  let value=line.slice(line.indexOf('=')+1).trim();
  if((value.startsWith('"')&&value.endsWith('"'))||(value.startsWith("'")&&value.endsWith("'"))) value=value.slice(1,-1);
  process.env.OPENROUTER_API_KEY=value;
}
process.env.PLASM_EVAL_PROVIDER='openrouter';
process.env.PLASM_REASONING_EFFORT='high';
process.env.PLASM_EVAL_MODEL_TRACE_DIR=path.join(out,'wire');
const model = resolveEvalModel('openai/gpt-6-luna');
fs.writeFileSync(path.join(out,'LAUNCHED'),new Date().toISOString(),{flag:'wx',mode:0o600});
for(const c of cases) {
  const start=performance.now();
  try {
    const result=await generateText({model,system,prompt:payload(c),temperature:0,maxOutputTokens:16384,maxRetries:0,abortSignal:AbortSignal.timeout(300000)});
    save(c.id+'.json',{id:c.id,text:result.text,reasoningText:result.reasoningText,usage:result.usage,providerMetadata:result.providerMetadata,finishReason:result.finishReason,warnings:result.warnings,responseId:result.response.id,responseModel:result.response.modelId,elapsedMs:performance.now()-start});
    console.log(c.id+' response saved');
  } catch(error) {
    save('ABORT.json',{id:c.id,error:String(error),elapsedMs:performance.now()-start});
    console.error('Unrecovered caller fault; evidence preserved in ABORT.json.');
    process.exitCode=1;
    break;
  }
}
for(const p of sources) if(lock[p]!==sha(p)) throw new Error('Frozen source drift after run: '+p);
