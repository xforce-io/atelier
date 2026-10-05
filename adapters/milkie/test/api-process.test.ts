import assert from 'node:assert/strict';
import { test } from 'node:test';
import { mkdtemp, mkdir, readFile, readdir, rm, symlink } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { parseStart, prepareContext } from '../src/api-process.js';
import { adapterFailureTerminal } from '../src/channel.js';
import { executeApiTurn } from '../src/api-turn.js';
import { ToolLedger } from '../src/tool-ledger.js';
import type { IModelGateway } from '@freemanxu/milkie';
import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';
import { createHash } from 'node:crypto';
import { once } from 'node:events';
import { fileURLToPath } from 'node:url';
import { milkieCommit } from '../src/channel.js';

const scope={taskId:'task-one',runId:'run-one',deliveryId:'delivery-one'};
async function fixture() {
  const root=await mkdtemp(join(tmpdir(),'atelier-process-'));
  const contextDirectory=join(root,'context'),ledgerDirectory=join(root,'ledger');
  await mkdir(contextDirectory,{mode:0o700});await mkdir(ledgerDirectory,{mode:0o700});
  const start=parseStart({workerId:'worker-one',contextId:'context-one',configurationId:'configuration-one',purposeFamily:'coordinate',contextDirectory,ledgerDirectory,resume:false,
    goal:'查询任务',input:'处理当前投递',skill:'只使用绑定工具',tools:[],connection:{protocol:'openai-chat-completions',model:'fixture-model',baseUrl:'https://example.invalid/v1',apiKey:'synthetic-test-secret'}});
  return {root,start};
}

test('deploy purpose family is accepted as its own adapter start',async()=>{
  const {root,start}=await fixture();
  try { assert.equal(parseStart({...start,purposeFamily:'deploy'}).purposeFamily,'deploy'); }
  finally { await rm(root,{recursive:true,force:true}); }
});

test('API process persists bound native context and explicitly resumes its checkpoint',async()=>{
  const {root,start}=await fixture();
  try {
    const context=await prepareContext(start,scope);
    const gateway:IModelGateway={complete:async()=>({content:[{type:'text',text:'等待规则'}],toolCalls:[],finishReason:'end_turn'}),async *stream(){throw new Error('not streaming');}};
    try {
      await executeApiTurn({workerId:start.workerId,taskId:scope.taskId,runId:scope.runId,contextId:start.contextId,goal:start.goal,input:start.input,skill:start.skill,
        model:{provider:'fixture',adapter:'fixture',model:'fixture'},tools:[],gateway,stateStore:context.store,eventStore:context.events,ledger:new ToolLedger(start.ledgerDirectory,scope.deliveryId),forward:async()=>{throw new Error('no tools');}});
    }finally {context.store.close();}
    await assert.rejects(prepareContext(start,scope),/already_exists/);
    const resumed=await prepareContext({...start,resume:true},{...scope,runId:'run-two'});
    assert.equal(resumed.checkpoint?.meta.contextId,start.contextId);resumed.store.close();
    for(const change of [{workerId:'other-worker'},{configurationId:'other-configuration'},{purposeFamily:'verify' as const}]) {
      await assert.rejects(prepareContext({...start,...change,resume:true},scope),/binding_mismatch/);
    }
    await assert.rejects(prepareContext({...start,resume:true},{...scope,taskId:'other-task'}),/binding_mismatch/);
    assert.equal((await readFile(join(start.contextDirectory,'binding.json'),'utf8')).includes('synthetic-test-secret'),false);
  }finally {await rm(root,{recursive:true,force:true});}
});

test('API native dialogue continues across deliveries without replaying the previous delivery ledger',async()=>{
  const {root,start}=await fixture();
  let effects=0;
  const tool={name:'message_respond',description:'处理当前消息',inputSchema:{type:'object',properties:{reason:{type:'string'}},required:['reason'],additionalProperties:false}};
  try {
    for(let turn=1;turn<=2;turn++) {
      const currentScope={...scope,runId:`run-${turn}`,deliveryId:`delivery-${turn}`};
      const context=await prepareContext({...start,resume:turn>1},currentScope);
      const seen:string[]=[];
      let requests=0;
      const gateway:IModelGateway={complete:async request=>{
        seen.push(JSON.stringify(request));requests++;
        if(requests===1)return {content:[{type:'tool_use',id:'same-native-call',name:tool.name,input:{reason:`message-${turn}`}}],toolCalls:[{id:'same-native-call',name:tool.name,input:{reason:`message-${turn}`}}],finishReason:'tool_use'};
        return {content:[{type:'text',text:`answer-${turn}`}],toolCalls:[],finishReason:'end_turn'};
      },async *stream(){throw new Error('not streaming');}};
      try {
        const ledger=await ToolLedger.openDelivery(start.ledgerDirectory,currentScope.deliveryId);
        const result=await executeApiTurn({workerId:start.workerId,taskId:scope.taskId,runId:currentScope.runId,contextId:start.contextId,
          goal:start.goal,input:`new-message-${turn}`,skill:start.skill,tools:[tool],gateway,model:{provider:'fixture',adapter:'fixture',model:'fixture'},
          stateStore:context.store,eventStore:context.events,checkpoint:context.checkpoint,ledger,
          forward:async operation=>{assert.equal(operation.originatingRunId,currentScope.runId);effects++;return {ok:true,receipt:`receipt-${turn}`};}});
        assert.equal(result.result.status,'completed');assert.equal(result.recoveredOperations,0);
        assert.equal(effects,turn);
        if(turn===2) {
          // milkie's native turn-end contract archives input + final answer;
          // tool scratchpad expires, while the operation ledger remains durable.
          assert.ok(seen[0]?.includes('new-message-1'));assert.ok(seen[0]?.includes('answer-1'));
          assert.ok(seen[0]?.includes('new-message-2'));
          const old=join(start.ledgerDirectory,'delivery-1');
          const record=JSON.parse(await readFile(join(old,(await readdir(old))[0]!),'utf8'));
          assert.deepEqual(record.result,{ok:true,receipt:'receipt-1'});
        }
      }finally{context.store.close();}
    }
  }finally{await rm(root,{recursive:true,force:true});}
});

test('missing native checkpoint continues only when no tool result was committed',async()=>{
  const {root,start}=await fixture();
  try {
    await assert.rejects(prepareContext({...start,resume:true},scope));
    const context=await prepareContext(start,scope);context.store.close();
    const continued=await prepareContext({...start,resume:true},scope);
    assert.equal(continued.checkpoint,undefined);
    continued.store.close();
    const ledger=await ToolLedger.openDelivery(start.ledgerDirectory,scope.deliveryId);
    await assert.rejects(ledger.call(scope.runId,'call-pending','task_read',{},async()=>{throw new Error('stop-before-result');}));
    const afterPending=await prepareContext({...start,resume:true},{...scope,runId:'run-pending'});
    assert.equal(afterPending.checkpoint,undefined);
    afterPending.store.close();
    await ledger.call(scope.runId,'call-done','task_read',{},async()=>({ok:true}));
    await assert.rejects(prepareContext({...start,resume:true},{...scope,runId:'run-two'}),/native_checkpoint_missing/);
    assert.deepEqual(adapterFailureTerminal(new Error('native_checkpoint_missing')),{stopReason:'failed',stopCode:'CHECKPOINT_MISSING',nativeStopReason:'native_checkpoint_missing',recoveredOperations:0});
    assert.equal(adapterFailureTerminal(new Error('adapter_start_invalid')).stopCode,'ADAPTER_FAILED');
    await symlink(start.contextDirectory,join(root,'linked-context'));
    await assert.rejects(prepareContext({...start,contextDirectory:join(root,'linked-context'),resume:true},scope),/directory_invalid/);
    await assert.rejects(prepareContext({...start,ledgerDirectory:start.contextDirectory+'/.',resume:true},scope),/must_be_separate/);
  }finally {await rm(root,{recursive:true,force:true});}
});

test('API start rejects ambient credential selection, unsafe endpoints and arbitrary configuration',async()=>{
  const {root,start}=await fixture();
  try {
    for(const change of [
      {actor:'other-worker'}, {connection:{...start.connection,apiKey:undefined}},
      {connection:{...start.connection,baseUrl:'https://name:secret@example.invalid/v1'}},
      {connection:{...start.connection,baseUrl:'http://example.invalid/v1'}},
      {connection:{...start.connection,baseUrl:'https://example.invalid/v1?token=synthetic'}},
      {connection:{...start.connection,runtime:'pi'}},
      {tools:[{name:'run_command',description:'not a valid core schema',inputSchema:{type:'object'}}]},
    ])assert.throws(()=>parseStart({...start,...change}),/start_invalid/);
  }finally {await rm(root,{recursive:true,force:true});}
});

test('production API child assembles explicit connection and cancels before calling the provider',async()=>{
  const {root,start}=await fixture();
  const child=spawn(process.execPath,[fileURLToPath(new URL('../src/main.js',import.meta.url)),'--atelier-run','00000000-0000-4000-8000-000000000001'],{env:{PATH:process.env.PATH??'',LOG_LEVEL:'silent'},stdio:['pipe','pipe','pipe']});
  let stderr='';child.stderr.on('data',chunk=>{stderr+=String(chunk);});
  const exited=once(child,'close');
  const lines=createInterface({input:child.stdout,crlfDelay:Infinity})[Symbol.asyncIterator]();
  const send=(seq:number,kind:string,payload:unknown)=>child.stdin.write(JSON.stringify({protocolVersion:1,...scope,seq,kind,requestId:kind,payload})+'\n');
  const timer=setTimeout(()=>child.kill('SIGKILL'),10_000);
  try {
    send(1,'hello',{milkieCommit,transport:'api',skillDigest:createHash('sha256').update(start.skill).digest('hex'),builtinToolsDisabled:true,stableToolCallIds:true,nativeCheckpoint:true,privateTools:true});
    const ready=JSON.parse((await lines.next()).value as string) as {kind:string};assert.equal(ready.kind,'ready');
    send(2,'start',start);send(3,'stop',{reason:'runtime_stop'});
    const terminal=JSON.parse((await lines.next()).value as string) as {kind:string;payload:{stopReason:string}};
    assert.equal(terminal.kind,'terminal');assert.equal(terminal.payload.stopReason,'cancelled');
    assert.equal((await exited)[0],0);assert.equal(stderr.includes('synthetic-test-secret'),false);
  }finally {clearTimeout(timer);child.kill('SIGKILL');await exited;await rm(root,{recursive:true,force:true});}
});
