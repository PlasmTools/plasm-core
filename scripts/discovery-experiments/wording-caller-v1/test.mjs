import assert from 'node:assert/strict';
import {cases, createState, apply, score, controls, validCall, dispositions} from './environment.mjs';

controls();
for(const call of [
  {toolName:'discover',input:{}},
  {toolName:'discover',input:{intent:'read'},invalid:true},
  {toolName:'finish',input:{outcome:'completed'}},
  {toolName:'finish',input:{outcome:'invented',value:'x',explanation:''}},
  {toolName:'execute',input:{operation:'read_status',extra:true}},
]) assert.equal(validCall(call),false);
assert.equal(validCall({toolName:'discover',input:{intent:'read'}}),true);
assert.equal(validCall({toolName:'finish',input:{outcome:'blocked',value:'',explanation:'unresolved'}}),true);
const c=cases[0],s=createState(c),terminal={outcome:'completed',value:'The status is stable'};
assert.equal(score(c,s,terminal,'terminal').prematureCompletion,true);
apply(c,s,'baseline','execute',{operation:c.target});
const observed=score(c,s,terminal,'terminal');
assert.equal(observed.correct,false);
assert.equal(observed.prematureCompletion,false);
assert.equal(observed.terminalValueMismatch,true);
const partial={id:'b',generations:[{usage:{inputTokens:12}}],status:'interrupted'};
const result=dispositions([{id:'a'},{id:'b'},{id:'c'}],[{id:'a'}],partial);
assert.deepEqual(result.unstarted,['c']);
assert.equal(result.completed,1);
assert.equal(result.interrupted[0].generations[0].usage.inputTokens,12);
assert.deepEqual(dispositions([{id:'a'}],[{id:'a'}],null).unstarted,[]);
console.log('PASS: invalid-call, observation, exact-answer and interruption controls.');
