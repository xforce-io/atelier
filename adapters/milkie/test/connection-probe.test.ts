import assert from 'node:assert/strict';
import {test} from 'node:test';
import {spawn} from 'node:child_process';
import {once} from 'node:events';
import {fileURLToPath} from 'node:url';
import type {IModelGateway,ModelResponse,ModelRequest} from '@freemanxu/milkie';
import {parseProbe,probeGateway} from '../src/connection-probe.js';

function gateway(complete:IModelGateway['complete']):IModelGateway {
  return {complete,async *stream(){throw new Error('stream must not be used');}};
}
test('connection probe makes one bounded no-tool model request and never exposes response content',async()=>{
  let calls=0;
  const actual=gateway(async(request:ModelRequest)=>{
    calls++;assert.equal(request.model,'exact-model');assert.equal(request.tools,undefined);
    assert.equal(request.messages.length,1);assert.equal(request.maxTokens,64);
    return {content:[{type:'text',text:'private-provider-response'}],toolCalls:[],finishReason:'stop',raw:{secret:'never-return'}};
  });
  const result=await probeGateway(actual,'exact-model',new AbortController().signal);
  assert.deepEqual(result,{state:'passed',code:'response_received'});assert.equal(calls,1);
  const aborted=new AbortController();aborted.abort();
  assert.equal((await probeGateway(actual,'exact-model',aborted.signal)).state,'inconclusive');assert.equal(calls,1);
});
test('provider errors, tool calls and empty responses do not become successful readiness',async()=>{
  const rejected=gateway(async()=>{throw {message:'secret-token',status:401,raw:{apiKey:'secret-token'}};});
  assert.deepEqual(await probeGateway(rejected,'model',new AbortController().signal),{state:'failed',code:'provider_rejected',httpStatus:401});
  for(const response of [
    {content:[],toolCalls:[]},
    {content:[{type:'text',text:'OK'}],toolCalls:[{id:'invented',name:'task_create',input:{}}]},
  ]) {
    assert.equal((await probeGateway(gateway(async()=>response as ModelResponse),'model',new AbortController().signal)).state,'inconclusive');
  }
  const failure=await probeGateway(gateway(async()=>{throw new Error('https://secret-token@example.invalid');}),'model',new AbortController().signal);
  assert.deepEqual(failure,{state:'failed',code:'connection_failed'});
});
test('probe configuration rejects ambient credentials and extra capability inputs',()=>{
  const valid={protocol:'openai-chat-completions',model:'model',apiKey:'synthetic-probe-secret',baseUrl:'https://example.invalid'};
  assert.equal(parseProbe(valid).model,'model');
  for(const changed of [{apiKey:undefined},{apiKey:'line\nbreak'},{env:{API_KEY:'x'}},{tools:[]},{baseUrl:'http://example.invalid'},{baseUrl:'https://name:secret@example.invalid'}])assert.throws(()=>parseProbe({...valid,...changed}));
});
test('real diagnostic child stops when its private input pipe closes and prints only sanitized result',async()=>{
  const child=spawn(process.execPath,[fileURLToPath(new URL('../src/probe-main.js',import.meta.url))],{env:{PATH:process.env.PATH??''},stdio:['pipe','pipe','pipe']});
  let stdout='',stderr='';child.stdout.on('data',c=>stdout+=String(c));child.stderr.on('data',c=>stderr+=String(c));
  const exited=once(child,'close');const deadline=setTimeout(()=>child.kill('SIGKILL'),5_000);
  try {
    child.stdin.end(JSON.stringify({protocol:'openai-chat-completions',model:'fixture',apiKey:'synthetic-probe-secret',baseUrl:'https://example.invalid'})+'\n');
    assert.equal((await exited)[0],0);assert.equal(JSON.parse(stdout).state,'inconclusive');
    assert.equal((stdout+stderr).includes('synthetic-probe-secret'),false);
  }finally {clearTimeout(deadline);child.kill('SIGKILL');await exited;}
});
test('real diagnostic child has a finite deadline even when parent never completes its input',{timeout:30_000},async()=>{
  const child=spawn(process.execPath,[fileURLToPath(new URL('../src/probe-main.js',import.meta.url))],{env:{PATH:process.env.PATH??'',LOG_LEVEL:'silent'},stdio:['pipe','pipe','pipe']});
  const exited=once(child,'close');let stdout='';child.stdout.on('data',c=>stdout+=String(c));
  const started=Date.now();const guard=setTimeout(()=>child.kill('SIGKILL'),28_000);
  try {
    assert.equal((await exited)[0],0);
    assert.deepEqual(JSON.parse(stdout),{state:'inconclusive',code:'deadline'});
    assert.ok(Date.now()-started<28_000);
  }finally {clearTimeout(guard);child.kill('SIGKILL');await exited;}
});
