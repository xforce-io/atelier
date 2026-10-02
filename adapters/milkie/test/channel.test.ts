import assert from 'node:assert/strict';
import { test } from 'node:test';
import { PassThrough } from 'node:stream';
import { createInterface } from 'node:readline';
import { createHash } from 'node:crypto';
import { AdapterChannel, milkieCommit } from '../src/channel.js';

const scope={taskId:'task',runId:'run',deliveryId:'delivery'};
const skill='由核心绑定身份，模型只选择获准操作。';
const caps={milkieCommit,transport:'api',skillDigest:createHash('sha256').update(skill).digest('hex'),builtinToolsDisabled:true,stableToolCallIds:true,nativeCheckpoint:true,privateTools:true};
function harness() {
  const input=new PassThrough(),output=new PassThrough();
  const iterator=createInterface({input:output,crlfDelay:Infinity})[Symbol.asyncIterator]();
  const send=(seq:number,kind:string,payload:unknown,requestId=kind,extra={})=>input.write(JSON.stringify({protocolVersion:1,...scope,seq,kind,payload,requestId,...extra})+'\n');
  const next=async()=>JSON.parse((await iterator.next()).value as string) as Record<string,unknown>;
  return {input,output,send,next};
}
async function started() {
  const h=harness();
  const connecting=AdapterChannel.connect(h.input,h.output);
  h.send(1,'hello',caps);
  const channel=await connecting;
  assert.equal((await h.next()).kind,'ready');
  const start=channel.start();h.send(2,'start',{skill});await start;
  return {...h,channel};
}

test('pipe binds scope and tool operation; duplicate core result is harmless',async()=>{
  const h=await started();
  const operation={operationId:'operation',originatingRunId:'run',toolCallId:'call',name:'task_read',input:{}};
  const promise=h.channel.forward(operation);
  const request=await h.next();
  assert.equal(request.kind,'tool.request');assert.equal(request.taskId,'task');assert.equal(request.requestId,'operation');
  h.send(3,'tool.result',{ok:true,data:{id:'task'}},'operation');
  assert.deepEqual(await promise,{ok:true,data:{id:'task'}});
  h.send(3,'tool.result',{ok:true,data:{id:'task'}},'operation');
  await h.channel.finish({stopReason:'completed',nativeStopReason:'model_stop',recoveredOperations:0});
  assert.equal((await h.next()).kind,'terminal');
  h.output.destroy();
});

test('core stop aborts in-flight tools and no subsequent tool is sent',async()=>{
  const h=await started();
  const operation={operationId:'operation',originatingRunId:'run',toolCallId:'call',name:'task_read',input:{}};
  const call=h.channel.forward(operation);
  const rejected=assert.rejects(call,/run_cancelled/);
  await h.next();h.send(3,'stop',{reason:'runtime_stop'});
  await rejected;
  assert.equal(h.channel.signal.aborted,true);
  await assert.rejects(h.channel.forward(operation),/run_cancelled/);
  await h.channel.finish({stopReason:'cancelled',nativeStopReason:'cancelled',recoveredOperations:0});
  assert.equal((await h.next()).kind,'terminal');h.output.destroy();
});

test('cross-task frame, skipped sequence and conflicting duplicate fail closed',async()=>{
  for(const variant of ['scope','gap','conflict']) {
    const h=await started();
    const call=h.channel.forward({operationId:'operation',originatingRunId:'run',toolCallId:'call',name:'task_read',input:{}});
    const rejected=assert.rejects(call,/protocol_invalid/);await h.next();
    if(variant==='scope') h.send(3,'tool.result',{ok:true},'operation',{taskId:'other'});
    if(variant==='gap') h.send(4,'tool.result',{ok:true},'operation');
    if(variant==='conflict') h.send(2,'start',{skill:'changed'});
    await rejected;assert.equal(h.channel.signal.aborted,true);h.output.destroy();
  }
});

test('wrong capability, changed Skill and oversized unframed data are rejected',async()=>{
  for(const variant of ['capability','skill','size']) {
    const h=harness();
    const connecting=AdapterChannel.connect(h.input,h.output);
    if(variant==='capability') {
      const rejected=assert.rejects(connecting,/protocol_invalid/);
      h.send(1,'hello',{...caps,builtinToolsDisabled:false});await rejected;
    } else {
      h.send(1,'hello',caps);const channel=await connecting;await h.next();
      const rejected=assert.rejects(channel.start(),/protocol_invalid/);
      if(variant==='skill')h.send(2,'start',{skill:'unexpected'});
      else h.input.write('x'.repeat(512*1024+1));
      await rejected;
    }
    h.output.destroy();
  }
});

test('partial incoming frame survives a stop-independent event and truncated EOF rejects pending operation',async()=>{
  const h=await started();
  const call=h.channel.forward({operationId:'operation',originatingRunId:'run',toolCallId:'call',name:'task_read',input:{}});
  const rejected=assert.rejects(call,/member_channel_closed/);await h.next();
  h.input.write('{"protocolVersion":1');h.input.end();await rejected;h.output.destroy();
});
