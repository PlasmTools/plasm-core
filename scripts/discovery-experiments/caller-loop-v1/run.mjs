import fs from 'node:fs';
import path from 'node:path';
import {fileURLToPath,pathToFileURL} from 'node:url';
import {createRequire} from 'node:module';
import {createHash} from 'node:crypto';
import {setTimeout as delay} from 'node:timers/promises';
const here=path.dirname(fileURLToPath(import.meta.url)), core=path.resolve(here,'../../..'), root=path.dirname(core);
if(!process.env.PLASM_EXPERIMENT_OUTPUT)throw Error('Set PLASM_EXPERIMENT_OUTPUT to a fresh evidence directory');
const out=path.resolve(process.env.PLASM_EXPERIMENT_OUTPUT);
if(process.argv.includes('--freeze')) { fs.mkdirSync(out,{recursive:true}); fs.copyFileSync(path.join(core,'fixtures/discovery/caller-loop-v1/cases.json'),path.join(out,'cases.json'),fs.constants.COPYFILE_EXCL); }
const fixture=path.join(core,'fixtures/discovery/caller-loop-v1'), agent=path.join(root,'scripts/appworld/agent');
const read=p=>fs.readFileSync(p,'utf8'), hash=p=>createHash('sha256').update(fs.readFileSync(p)).digest('hex');
const save=(name,value)=>fs.writeFileSync(path.join(out,name),JSON.stringify(value,null,2)+'\n',{mode:0o600});
const walk=p=>fs.readdirSync(p,{withFileTypes:true}).flatMap(x=>x.isDirectory()?walk(path.join(p,x.name)):[path.join(p,x.name)]);
const sources=[...walk(fixture),...['plasm-core','plasm-agent-core','plasm-catalog'].flatMap(c=>walk(path.join(core,'crates',c,'src')).filter(p=>p.endsWith('.rs'))),path.join(out,'cases.json'),fileURLToPath(import.meta.url),path.join(here,'audit.py'),path.join(here,'preflight.mjs'),...['resolve-eval-model.ts','provider-throttle.ts','model-wire-trace.ts'].map(p=>path.join(agent,'src',p)),path.join(agent,'package.json'),path.join(agent,'package-lock.json')];
if(process.argv.includes('--freeze')) {
  if(fs.existsSync(path.join(out,'LOCK.json')))throw Error('Already frozen');
  save('LOCK.json',Object.fromEntries(sources.map(p=>[p,hash(p)])));
  console.log('Frozen caller-loop inputs, sources and judge.');process.exit(0);
}
const lock=JSON.parse(read(path.join(out,'LOCK.json')));
const verify=()=>{for(const p of sources)if(lock[p]!==hash(p))throw Error('Source drift: '+p);};verify();
const cases=JSON.parse(read(path.join(out,'cases.json'))),system=read(path.join(fixture,'PROMPT.txt'));
if(cases.length!==10||new Set(cases.map(c=>c.id)).size!==10)throw Error('Case inventory');
const require=createRequire(path.join(agent,'package.json'));
const {generateText,tool}=await import(pathToFileURL(require.resolve('ai')).href);
const {z}=await import(pathToFileURL(require.resolve('zod')).href);
const {resolveEvalModel}=await import(pathToFileURL(path.join(agent,'src/resolve-eval-model.ts')).href);
const tools={plasm_context:tool({description:'Discover capabilities for a self-contained objective. Returns bounded judgments and exposed declarations.',inputSchema:z.object({intent:z.string().min(1),objective_transition:z.enum(['continue','revise'])}).strict()})};
if(process.argv.includes('--imports')){console.log('PASS: frozen sources and existing caller/tool schema imports. No provider calls.');process.exit(0);}
const ready=JSON.parse(read(path.join(out,'READY.json')));
if(ready.controls.length!==10||ready.controls.some(c=>c.first===null||c.first>6))throw Error('Native upper-bound control failed');
if(process.argv.includes('--preflight')){console.log('PASS: all ten native controls available within six; sources and imports verified.');process.exit(0);}
if(!process.argv.includes('--run'))throw Error('Specify mode');
if(!process.env.OPENROUTER_API_KEY){
 const line=read(path.join(root,'.env')).split('\n').map(x=>x.trim().replace(/^export\s+/,'')) .find(x=>x.startsWith('OPENROUTER_API_KEY='));
 if(!line)throw Error('Missing configured provider credential');
 let v=line.slice(line.indexOf('=')+1).trim();if((v.startsWith('"')&&v.endsWith('"'))||(v.startsWith("'")&&v.endsWith("'")))v=v.slice(1,-1);process.env.OPENROUTER_API_KEY=v;
}
process.env.PLASM_EVAL_PROVIDER='openrouter';process.env.PLASM_REASONING_EFFORT='high';process.env.PLASM_EVAL_MODEL_TRACE_DIR=path.join(out,'wire');
const model=resolveEvalModel('openai/gpt-6-luna');
const initial='Apply a violet inspection seal to specimen units. Preserve their existing measurements and identifiers.';
const amended='Apply an amber coating to specimen units instead of a violet inspection seal. Preserve their existing measurements and identifiers.';
let serial=0;let active=null;
fs.writeFileSync(path.join(out,'LAUNCHED'),new Date().toISOString(),{flag:'wx',mode:0o600});
try{
 for(const c of cases){
  active=c.id;let messages=[{role:'user',content:initial}],turns=[],status='exhausted';
  for(let turn=1;turn<=6;turn++){
   if(c.amend_at===turn)messages.push({role:'user',content:amended});
   if(c.observation_at===turn)messages.push({role:'user',content:'Unit X has an incomplete measurement record. The existing measurements and identifiers must remain intact.'});
   const start=performance.now();let result;
   try{result=await generateText({model,system,messages,tools,maxOutputTokens:16384,temperature:0,maxRetries:0,abortSignal:AbortSignal.timeout(300000)});}
   catch(error){if(['AI_InvalidToolInputError','AI_NoSuchToolError'].includes(error.name)){status='invalid_tool_call';turns.push({turn,error:String(error)});break;}throw error;}
   const raw={id:c.id,turn,text:result.text,toolCalls:result.toolCalls,usage:result.usage,providerMetadata:result.providerMetadata,warnings:result.warnings,finishReason:result.finishReason,responseId:result.response.id,responseModel:result.response.modelId,elapsedMs:performance.now()-start,messages:result.response.messages};
   save(`${c.id}-${turn}.model.json`,raw);
   if(result.toolCalls.length===0){status='premature_stop';turns.push({turn,text:result.text});break;}
   if(result.toolCalls.length!==1||result.toolCalls[0].toolName!=='plasm_context'){status='invalid_tool_call';turns.push({turn});break;}
   const call=result.toolCalls[0],input=call.input;
   if(typeof input?.intent!=='string'||!input.intent.trim()||!['continue','revise'].includes(input.objective_transition)||Object.keys(input).sort().join(',')!=='intent,objective_transition'){status='invalid_tool_call';turns.push({turn});break;}
   const number=String(serial).padStart(3,'0'),requestName=`${number}.request.json`;
   const request={id:c.id,intent:input.intent,signal:input.objective_transition};
   if(fs.existsSync(path.join(out,requestName)))throw Error('Duplicate native request');
   save(requestName+'.tmp',request);fs.renameSync(path.join(out,requestName+'.tmp'),path.join(out,requestName));
   const responsePath=path.join(out,`${number}.response.json`),deadline=Date.now()+60000;
   while(!fs.existsSync(responsePath)){if(Date.now()>deadline)throw Error('Native response deadline');await delay(100);}
   const response=JSON.parse(read(responsePath));serial++;
   turns.push({turn,...request,expected_signal:turn===1||c.amend_at===turn?'revise':'continue',native:response});
   save(c.id+'.json',{id:c.id,status:'running',turns});
   if(response.found){status='available';break;}
   messages.push(...result.response.messages);
   messages.push({role:'tool',content:[{type:'tool-result',toolCallId:call.toolCallId,toolName:'plasm_context',output:{type:'json',value:{exposed:[],judgments:response.judged,more_candidates_exist:response.more_candidates_exist}}}]});
  }
  save(c.id+'.json',{id:c.id,status,turns});console.log(c.id+' '+status+' after '+turns.length+' calls');
 }
 verify();save('FINISHED.json',{at:new Date().toISOString(),native_requests:serial});
}catch(error){save('ABORT.json',{id:active,error:String(error),native_requests:serial});console.error('Unrecovered fault; see ABORT.json.');process.exitCode=1;}
finally{fs.writeFileSync(path.join(out,'STOP'),'stop\n',{flag:'wx',mode:0o600});}
