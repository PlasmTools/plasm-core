import fs from 'node:fs';
import path from 'node:path';
import {fileURLToPath,pathToFileURL} from 'node:url';
import {createRequire} from 'node:module';
import {createHash} from 'node:crypto';
import {cases,system,createState,surface,apply,score,controls,validCall,dispositions} from './environment.mjs';

const here=path.dirname(fileURLToPath(import.meta.url)),core=path.resolve(here,'../../..'),root=path.dirname(core);
if(!process.env.PLASM_EXPERIMENT_OUTPUT)throw Error('Set PLASM_EXPERIMENT_OUTPUT to a new evidence directory');
const out=path.resolve(process.env.PLASM_EXPERIMENT_OUTPUT),agent=path.join(root,'scripts/appworld/agent');
const read=p=>fs.readFileSync(p,'utf8'),sha=p=>createHash('sha256').update(fs.readFileSync(p)).digest('hex');
const save=(name,data)=>fs.writeFileSync(path.join(out,name),JSON.stringify(data,null,2)+'\n',{flag:'wx',mode:0o600});
const sources=[fileURLToPath(import.meta.url),path.join(here,'environment.mjs'),path.join(core,'fixtures/discovery/wording-caller-v1/PREREGISTRATION.md'),path.join(core,'packages/plasm-agent/src/tools/format.ts'),...['resolve-eval-model.ts','provider-throttle.ts','model-wire-trace.ts'].map(p=>path.join(agent,'src',p)),path.join(agent,'package.json'),path.join(agent,'package-lock.json')];
const plan=[];
for(let repeat=0;repeat<2;repeat++)for(const [i,c] of cases.entries())for(const arm of (i+repeat)%2?['candidate','baseline']:['baseline','candidate'])plan.push({id:`${c.id}-${repeat}-${arm}`,case:c.id,repeat,arm});
controls();
if(process.argv.includes('--freeze')) {
  fs.mkdirSync(out,{recursive:false});
  save('LOCK.json',Object.fromEntries(sources.map(p=>[p,sha(p)])));
  save('CASES.json',cases);save('PLAN.json',plan);save('SYSTEM.json',{system});
  console.log('Frozen 20 trajectories; offline controls pass.');process.exit(0);
}
const lock=JSON.parse(read(path.join(out,'LOCK.json')));
const verify=()=>{for(const p of sources)if(lock[p]!==sha(p))throw Error('Source drift: '+p);};verify();
const require=createRequire(path.join(agent,'package.json'));
const {generateText,tool}=await import(pathToFileURL(require.resolve('ai')).href);
const {z}=await import(pathToFileURL(require.resolve('zod')).href);
const {resolveEvalModel}=await import(pathToFileURL(path.join(agent,'src/resolve-eval-model.ts')).href);
const tools={
  discover:tool({description:'Request operation declarations for the current objective. Does not read account data or execute operations.',inputSchema:z.object({intent:z.string().min(1)}).strict()}),
  execute:tool({description:'Execute an available declared operation in this environment and observe its result.',inputSchema:z.object({operation:z.string().min(1)}).strict()}),
  finish:tool({description:'Report the requested observed value, or why the request remains unresolved. Unavailable asserts the required operation does not exist; blocked means you could not establish a way to complete the request.',inputSchema:z.object({outcome:z.enum(['completed','blocked','unavailable']),value:z.string(),explanation:z.string()}).strict()}),
};
if(process.argv.includes('--preflight')){console.log('PASS: frozen source, environment controls and SDK imports; no provider calls.');process.exit(0);}
if(!process.argv.includes('--run'))throw Error('Specify --freeze, --preflight or --run');
if(!process.env.OPENROUTER_API_KEY){
 const line=read(path.join(root,'.env')).split('\n').map(x=>x.trim().replace(/^export\s+/,'' )).find(x=>x.startsWith('OPENROUTER_API_KEY='));
 if(!line)throw Error('Configured provider credential missing');
 let value=line.slice(line.indexOf('=')+1).trim();if((value.startsWith('"')&&value.endsWith('"'))||(value.startsWith("'")&&value.endsWith("'")))value=value.slice(1,-1);
 process.env.OPENROUTER_API_KEY=value;
}
process.env.PLASM_EVAL_PROVIDER='openrouter';process.env.PLASM_REASONING_EFFORT='high';process.env.PLASM_EVAL_MODEL_TRACE_DIR=path.join(out,'wire');
const model=resolveEvalModel('openai/gpt-6-luna');
fs.writeFileSync(path.join(out,'LAUNCHED'),new Date().toISOString(),{flag:'wx',mode:0o600});
const rows=[];let active=null,interrupted=null;
try {
 for(const trial of plan){
  const c=cases.find(c=>c.id===trial.case),s=createState(c);
  let status='exhausted',terminal=null;const turns=[];
  active={...trial,generations:turns,calls:s.calls,executed:s.executed};
  const messages=[{role:'user',content:c.task},{role:'assistant',content:[{type:'tool-call',toolCallId:'initial-discovery',toolName:'discover',input:{intent:c.task}}]},
    {role:'tool',content:[{type:'tool-result',toolCallId:'initial-discovery',toolName:'discover',output:{type:'text',value:surface(c,s,trial.arm)}}]}];
  save(trial.id+'.INPUT.json',{trial,task:c.task,initialMessages:messages});
  for(let turn=1;turn<=6;turn++){
   const start=performance.now();
   const result=await generateText({model,system,messages,tools,temperature:0,maxOutputTokens:16384,maxRetries:0,abortSignal:AbortSignal.timeout(300000)});
   const elapsedMs=performance.now()-start;
   save(`${trial.id}-${turn}.MODEL.json`,{text:result.text,reasoningText:result.reasoningText,toolCalls:result.toolCalls,messages:result.response.messages,usage:result.usage,providerMetadata:result.providerMetadata,responseId:result.response.id,responseModel:result.response.modelId,warnings:result.warnings,finishReason:result.finishReason,elapsedMs});
   turns.push({turn,elapsedMs,usage:result.usage});messages.push(...result.response.messages);
   if(result.toolCalls.length!==1){status=result.toolCalls.length===0?'text_stop':'invalid_multiple_calls';break;}
   const call=result.toolCalls[0];
   if(!validCall(call)){status='invalid_output';break;}
   if(call.toolName==='finish'){terminal=call.input;status='terminal';break;}
   if(!['discover','execute'].includes(call.toolName)){status='invalid_tool';break;}
   const response=apply(c,s,trial.arm,call.toolName,call.input);
   messages.push({role:'tool',content:[{type:'tool-result',toolCallId:call.toolCallId,toolName:call.toolName,output:typeof response==='string'?{type:'text',value:response}:{type:'json',value:response}}]});
  }
  const row={...trial,...score(c,s,terminal,status),terminal,turns:turns.length,generations:turns,calls:s.calls,executed:s.executed};
  save(trial.id+'.RESULT.json',row);rows.push(row);console.log(`${trial.id}: ${status}, correct=${row.correct}, searches=${row.searches}, turns=${row.turns}`);
  active=null;
 }
 verify();save('FINISHED.json',{at:new Date().toISOString(),completed:rows.length});
}catch(error){interrupted=active?{...active,status:'interrupted',turns:active.generations.length}:null;if(interrupted)save(interrupted.id+'.INTERRUPTED.json',interrupted);save('ABORT.json',{at:new Date().toISOString(),trial:active?.id??null,error:String(error),completed:rows.length});console.error('Unrecovered failure; ABORT.json preserved.');process.exitCode=1;}
const summary={developmentOnly:true,planned:plan.length,...dispositions(plan,rows,interrupted),arms:Object.fromEntries(['baseline','candidate'].map(arm=>{
 const selected=rows.filter(r=>r.arm===arm);
 const measured=interrupted?.arm===arm?[...selected,interrupted]:selected;
 return [arm,{completed:selected.length,correct:selected.filter(r=>r.correct).length,
 discoveryCalls:measured.reduce((n,r)=>n+r.calls.filter(c=>c.name==='discover').length,0),
 postExposureSearches:selected.reduce((n,r)=>n+r.postExposureSearches,0),
 falseUnavailable:selected.filter(r=>r.falseUnavailable).length,
 prematureCompletion:selected.filter(r=>r.prematureCompletion).length,
 terminalValueMismatch:selected.filter(r=>r.terminalValueMismatch).length,
 missedUsefulContinuation:selected.filter(r=>r.missedUsefulContinuation).length,
 turns:measured.reduce((n,r)=>n+r.turns,0),
 generations:measured.flatMap(r=>r.generations)}];
})),rows};
save('SUMMARY.json',summary);
