// Offline SDK tool-history proof. This process cannot use the real network.
import assert from 'node:assert/strict';
import {createRequire} from 'node:module';
import {pathToFileURL,fileURLToPath} from 'node:url';
import path from 'node:path';
const root=path.resolve(path.dirname(fileURLToPath(import.meta.url)),'../../../..');
const agent=path.join(root,'scripts/appworld/agent');
const require=createRequire(path.join(agent,'package.json'));
const {generateText,tool}=await import(pathToFileURL(require.resolve('ai')).href);
const {z}=await import(pathToFileURL(require.resolve('zod')).href);
const {resolveEvalModel}=await import(pathToFileURL(path.join(agent,'src/resolve-eval-model.ts')).href);
process.env.OPENROUTER_API_KEY='offline-fixture';process.env.PLASM_EVAL_PROVIDER='openrouter';process.env.PLASM_REASONING_EFFORT='high';delete process.env.PLASM_EVAL_MODEL_TRACE_DIR;
let calls=0;
globalThis.fetch=async (_url,init)=>{
 const body=JSON.parse(init.body);calls++;
 if(calls===2){assert.equal(body.messages.at(-1).role,'tool');assert.equal(JSON.parse(body.messages.at(-1).content).more_candidates_exist,true);}
 return new Response(JSON.stringify({id:'offline-'+calls,object:'chat.completion',created:0,model:'offline',choices:[{index:0,message:{role:'assistant',content:null,tool_calls:[{id:'call-'+calls,type:'function',function:{name:'plasm_context',arguments:JSON.stringify({intent:'Inspect abstract units without changes.',objective_transition:calls===1?'revise':'continue'})}}]},finish_reason:'tool_calls'}],usage:{prompt_tokens:1,completion_tokens:1,total_tokens:2}}),{headers:{'content-type':'application/json'}});
};
const model=resolveEvalModel('openai/gpt-6-luna');
const tools={plasm_context:tool({inputSchema:z.object({intent:z.string(),objective_transition:z.enum(['continue','revise'])}).strict()})};
const messages=[{role:'user',content:'Inspect abstract units without changes.'}];
const first=await generateText({model,messages,tools,maxRetries:0});assert.equal(first.toolCalls.length,1);
messages.push(...first.response.messages);
messages.push({role:'tool',content:[{type:'tool-result',toolCallId:first.toolCalls[0].toolCallId,toolName:'plasm_context',output:{type:'json',value:{more_candidates_exist:true}}}]});
const second=await generateText({model,messages,tools,maxRetries:0});assert.equal(second.toolCalls[0].input.objective_transition,'continue');assert.equal(calls,2);
console.log('PASS: two offline caller turns preserve tool-call IDs, structured arguments and tool results; no provider calls.');
