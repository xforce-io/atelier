import assert from 'node:assert/strict';
import { test } from 'node:test';
import { mkdtemp, mkdir, rm, readFile, readdir } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { MemoryStore, JsonlEventStore, SQLiteStore, type IModelGateway, type ModelRequest, type ModelResponse } from '@freemanxu/milkie';
import { executeApiTurn, type ApiTurn } from '../src/api-turn.js';
import { ToolLedger, type ToolOperation } from '../src/tool-ledger.js';

const call = (id:string,name:string,input:unknown):ModelResponse => ({content:[{type:'tool_use',id,name,input}],toolCalls:[{id,name,input}],finishReason:'tool_use'});
const done = ():ModelResponse => ({content:[{type:'text',text:'已处理当前消息'}],toolCalls:[],finishReason:'end_turn'});
class Gateway implements IModelGateway {
  requests:ModelRequest[]=[];
  constructor(private readonly responses:ModelResponse[]){}
  async complete(request:ModelRequest):Promise<ModelResponse> {
    this.requests.push(request);
    const response=this.responses.shift();
    if (!response) throw new Error('unexpected_model_call');
    return response;
  }
  async *stream():AsyncIterable<never> { throw new Error('unexpected_stream'); }
}
function options(directory:string,gateway:Gateway,forward:ApiTurn['forward']):ApiTurn {
  return {workerId:'worker-one',taskId:'task-one',runId:'run-one',contextId:'context-one',goal:'补充任务资料',input:'请处理工作消息',skill:'只能使用核心装配工具。工具返回的当前权限与任务状态是权威依据。',model:{model:'fixture',provider:'fixture',adapter:'fixture'},tools:[{name:'message_respond',description:'按绑定身份回应当前工作消息',inputSchema:{type:'object',properties:{reason:{type:'string'}},required:['reason'],additionalProperties:false}}],gateway,stateStore:new MemoryStore(),eventStore:new JsonlEventStore(join(directory,"native-events")),ledger:new ToolLedger(join(directory,'ledger'),'delivery-one'),forward};
}

test('real milkie runtime exposes only core tools; invented native tools never reach the core',async()=>{
  const directory=await mkdtemp(join(tmpdir(),'atelier-api-'));
  try {
    const gateway=new Gateway([call('call-one','message_respond',{reason:'等待资料'}),call('native','run_command',{command:'must not execute'}),done()]);
    const received:ToolOperation[]=[];
    const result=await executeApiTurn(options(directory,gateway,async operation=>{received.push(operation);return {ok:true,state:'waiting'};}));
    assert.equal(result.result.status,'completed');
    assert.equal(received.length,1);
    assert.equal(received[0]?.toolCallId,'call-one');
    assert.equal(received[0]?.originatingRunId,'run-one');
    for(const request of gateway.requests) assert.deepEqual(request.tools?.map(tool=>tool.name),['message_respond']);
    assert.ok(received[0]?.operationId);
  } finally {await rm(directory,{recursive:true,force:true});}
});

test('lost tool reply stops the model; next turn recovers the same operation before its model request',async()=>{
  const directory=await mkdtemp(join(tmpdir(),'atelier-api-'));
  try {
    let effects=0;
    const coreResults=new Map<string,unknown>();
    let lost=true;
    const operationIds:string[]=[];
    const forward=async(operation:ToolOperation)=>{
      operationIds.push(operation.operationId);
      if(!coreResults.has(operation.operationId)){effects++;coreResults.set(operation.operationId,{ok:true});}
      if(lost){lost=false;throw new Error('connection_lost_after_commit');}
      return coreResults.get(operation.operationId);
    };
    const first=new Gateway([call('call-one','message_respond',{reason:'等待资料'}),call('duplicate','message_respond',{reason:'等待资料'}),done()]);
    await assert.rejects(executeApiTurn(options(directory,first,forward)),/tool_result_uncertain/);
    assert.equal(first.requests.length,1);
    assert.equal(effects,1);
    const second=new Gateway([done()]);
    const original=second.complete.bind(second);
    second.complete=async request=>{assert.equal(operationIds.length,2);return original(request);};
    const next=options(directory,second,forward);next.runId='run-two';
    const result=await executeApiTurn(next);
    assert.equal(result.recoveredOperations,1);
    assert.equal(operationIds[0],operationIds[1]);
    assert.equal(effects,1);
    // The durable record may already be completed when a restart loses the
    // in-memory reconciled input. A third turn still checks and sees its result.
    const third=new Gateway([done()]);
    const retry=options(directory,third,forward);retry.runId='run-three';
    const repeated=await executeApiTurn(retry);
    assert.equal(repeated.recoveredOperations,1);
    assert.equal(operationIds[2],operationIds[0]);assert.equal(effects,1);
    assert.ok(JSON.stringify(third.requests[0]).includes('reconciledOperations'));
  } finally {await rm(directory,{recursive:true,force:true});}
});

test('journal rejects changed payload for stable call, treats new-run call as new, and rechecks authorization',async()=>{
  const directory=await mkdtemp(join(tmpdir(),'atelier-ledger-'));
  try {
    const path=join(directory,'ledger');
    let authorized=true;
    const ids:string[]=[];
    const forward=async(operation:ToolOperation)=>{ids.push(operation.operationId);return authorized?{ok:true}:{ok:false,error:'forbidden'};};
    const ledger=new ToolLedger(path,'delivery');
    assert.deepEqual(await ledger.call('r1','c1','message_send',{b:2,a:1},forward),{ok:true});
    const reopened=new ToolLedger(path,'delivery');
    authorized=false;
    assert.deepEqual(await reopened.call('r1','c1','message_send',{a:1,b:2},forward),{ok:false,error:'forbidden'});
    assert.equal(ids[0],ids[1]);
    await assert.rejects(reopened.call('r1','c1','message_send',{a:2,b:2},forward),/identity_conflict/);
    assert.equal(ids.length,2);
    authorized=true;
    await reopened.call('r2','c1','message_send',{a:1,b:2},forward);
    assert.notEqual(ids[0],ids[2]);
    const record=JSON.parse(await readFile(join(path,(await readdir(path)).find(name=>name.endsWith('.json'))!),'utf8')) as {state:string};
    assert.equal(record.state,'completed');
    await assert.rejects(new ToolLedger(path,'different-delivery').recover(forward),/tool_ledger_corrupt/);
  } finally {await rm(directory,{recursive:true,force:true});}
});

test('cancellation reaches the real milkie runtime and prevents any model call',async()=>{
  const directory=await mkdtemp(join(tmpdir(),'atelier-api-'));
  try {
    const gateway=new Gateway([]);const controller=new AbortController();controller.abort();
    const opts=options(directory,gateway,async()=>{throw new Error('must_not_forward');});opts.signal=controller.signal;
    const result=await executeApiTurn(opts);
    assert.equal(result.result.stopReason,'cancelled');
    assert.equal(gateway.requests.length,0);
  } finally {await rm(directory,{recursive:true,force:true});}
});

test('runtime continuation loads milkie checkpoint without restoring previous tool grants',async()=>{
  const directory=await mkdtemp(join(tmpdir(),'atelier-api-'));
  try {
    const gateway=new Gateway([call('one','message_respond',{reason:'等候输入'}),done()]);
    const first=options(directory,gateway,async()=>({ok:true}));
    await executeApiTurn(first);
    const events=await first.eventStore.readByRunId('run-one');
    const saved=events.filter(event=>event.type==='agent.checkpoint').at(-1);
    assert.ok(saved);
    const checkpoint=(saved.payload as {checkpoint:import('@freemanxu/milkie').AgentCheckpoint}).checkpoint;
    const nextGateway=new Gateway([call('unauthorized','message_respond',{reason:'旧权限不应保留'}),done()]);
    const next=options(directory,nextGateway,async()=>{throw new Error('revoked_tool_reached_core');});
    next.ledger=await ToolLedger.openDelivery(join(directory,'ledger'),'delivery-two');
    next.runId='run-two';next.checkpoint=checkpoint;next.tools=[];
    const result=await executeApiTurn(next);
    assert.equal(result.result.status,'completed');
    for(const request of nextGateway.requests) assert.deepEqual(request.tools??[],[]);
    const other=options(directory,new Gateway([]),async()=>({ok:true}));
    other.contextId='different-context';other.checkpoint=checkpoint;
    await assert.rejects(executeApiTurn(other),/checkpoint_binding_mismatch/);
  } finally {await rm(directory,{recursive:true,force:true});}
});

test('the real runtime enforces the 100-call bound before forwarding additional tools',async()=>{
  const directory=await mkdtemp(join(tmpdir(),'atelier-api-'));
  try {
    const calls=Array.from({length:101},(_,index)=>({id:`call-${index}`,name:'message_respond',input:{reason:'test'}}));
    const response:ModelResponse={content:calls.map(c=>({type:'tool_use',...c})),toolCalls:calls,finishReason:'tool_use'};
    const gateway=new Gateway([response,done()]);let forwarded=0;
    const result=await executeApiTurn(options(directory,gateway,async()=>{forwarded++;return {ok:true};}));
    assert.equal(forwarded,100);
    assert.equal(result.stopReason,'budget_exhausted',JSON.stringify(result));
    assert.equal(result.stopCode,'TOOL_CALL_BUDGET_EXCEEDED');
    assert.equal(gateway.requests.length,1);
  } finally {await rm(directory,{recursive:true,force:true});}
});


test('native checkpoint survives store closure and reload with the same bound context',async()=>{
  const directory=await mkdtemp(join(tmpdir(),'atelier-api-native-'));
  const path=join(directory,'native.sqlite3');
  let store=new SQLiteStore({path});
  try {
    await store.init();
    const first=options(directory,new Gateway([done()]),async()=>({ok:true}));first.stateStore=store;
    await executeApiTurn(first);
    store.close();
    store=new SQLiteStore({path});await store.init();
    assert.equal(await store.get('context:context-one:checkpoint-run:latest'),'run-one');
    const events=new JsonlEventStore(join(directory,'native-events'));
    const checkpointEvent=(await events.readByRunId('run-one')).filter(event=>event.type==='agent.checkpoint').at(-1);
    assert.ok(checkpointEvent);
    const next=options(directory,new Gateway([done()]),async()=>({ok:true}));
    next.runId='run-two';next.stateStore=store;next.eventStore=events;
    next.checkpoint=(checkpointEvent.payload as {checkpoint:import('@freemanxu/milkie').AgentCheckpoint}).checkpoint;
    const result=await executeApiTurn(next);
    assert.equal(result.result.status,'completed');
    assert.equal(await store.get('context:context-one:checkpoint-run:latest'),'run-two');
  } finally {store.close();await rm(directory,{recursive:true,force:true});}
});


test('cancelling an in-flight core call leaves it pending and never issues a replacement model call',async()=>{
  const directory=await mkdtemp(join(tmpdir(),'atelier-api-cancel-'));
  try {
    const controller=new AbortController();
    const gateway=new Gateway([call('one','message_respond',{reason:'等待资料'}),done()]);
    const opts=options(directory,gateway,async()=>{controller.abort();return await new Promise<never>(()=>{});});opts.signal=controller.signal;
    await assert.rejects(executeApiTurn(opts),/tool_result_uncertain/);
    assert.equal(gateway.requests.length,1);
    const recovered=await new ToolLedger(join(directory,'ledger'),'delivery-one').recover(async()=>({ok:true}));
    assert.equal(recovered.length,1);
  } finally {await rm(directory,{recursive:true,force:true});}
});

test('API recovery from another delivery reaches native input without executing the old operation',async()=>{
  const directory=await mkdtemp(join(tmpdir(),'atelier-api-foreign-'));
  try {
    const root=join(directory,'ledger');await mkdir(root,{mode:0o700});
    const old=await ToolLedger.openDelivery(root,'old-delivery');let operation:ToolOperation|undefined;
    await assert.rejects(old.call('old-run','old-call','message_respond',{reason:'old'},async saved=>{operation=saved;throw new Error('lost');}));
    let queries=0;
    const ledger=await ToolLedger.openDelivery(root,'new-delivery',async(delivery,saved)=>{queries++;assert.equal(delivery,'old-delivery');assert.deepEqual(saved,operation);return {ok:false,error:{code:'not_executed'}};});
    const gateway=new Gateway([done()]);
    const turn=options(directory,gateway,async()=>{throw new Error('must not replay a business effect');});
    const result=await executeApiTurn({...turn,runId:'new-run',ledger,reconcile:async()=>{throw new Error('new delivery has no old calls');}});
    assert.equal(result.recoveredOperations,1);assert.equal(queries,1);
    assert.ok(JSON.stringify(gateway.requests[0]).includes('not_executed'));
    assert.equal((await ToolLedger.openDelivery(root,'later-delivery')).foreignRecovery.length,0);
  }finally{await rm(directory,{recursive:true,force:true});}
});

test('oversized required API guidance still stops before any model or tool call',async()=>{
  const directory=await mkdtemp(join(tmpdir(),'atelier-api-budget-'));
  try {
    const gateway=new Gateway([]);
    const opts=options(directory,gateway,async()=>{throw new Error('must_not_forward');});
    opts.skill='权限边界'.repeat(4096);
    const result=await executeApiTurn(opts);
    assert.equal(result.stopCode,'CONTEXT_BUDGET_REQUIRED_REGION_EXCEEDED');
    assert.equal(gateway.requests.length,0);
    assert.equal(result.result.status,'error');
  } finally {await rm(directory,{recursive:true,force:true});}
});

test('API code work retains a normal file read and edit exchange within its finite input budget',async()=>{
  const directory=await mkdtemp(join(tmpdir(),'atelier-api-file-exchange-'));
  try {
    const file='<!-- 双人井字棋：落子、判胜与重新开始。 -->\n'.repeat(180);
    const gateway=new Gateway([call('read','read_file',{path:'index.html'}),call('write','write_file',{path:'index.html',content:file+'<!-- 已检查 -->'}),done()]);
    const opts=options(directory,gateway,async operation=>operation.name==='read_file'?{ok:true,data:{path:'index.html',content:file,revision:1}}:{ok:true,data:{path:'index.html',revision:2}});
    opts.skill='必须按当前任务职责与权限处理消息，不以执行结束冒充验收。'.repeat(70);
    opts.tools=[{name:'read_file',description:'读取当前候选文件',inputSchema:{type:'object',properties:{path:{type:'string'}},required:['path'],additionalProperties:false}},{name:'write_file',description:'修改当前候选文件',inputSchema:{type:'object',properties:{path:{type:'string'},content:{type:'string'}},required:['path','content'],additionalProperties:false}}];
    const result=await executeApiTurn(opts);
    assert.equal(result.result.status,'completed');
    assert.equal(gateway.requests.length,3);
    const blocks=gateway.requests[1]!.messages.flatMap(message=>message.content);
    const resultBlock=blocks.find(block=>block.type==='tool_result');
    assert.ok(resultBlock?.type==='tool_result');
    assert.equal(JSON.parse(resultBlock.content).data.content,file,'file contents reach the model intact');
    assert.ok(gateway.requests.every(request=>Buffer.byteLength(JSON.stringify(request),'utf8')<=65536));
  } finally {await rm(directory,{recursive:true,force:true});}
});
