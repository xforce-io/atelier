import assert from 'node:assert/strict';
import { test } from 'node:test';
import { mkdtemp, mkdir, readFile, rm, symlink } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { parseStart, prepareContext } from '../src/api-process.js';
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

test('missing native checkpoint and symlink directory do not silently start a fresh context',async()=>{
  const {root,start}=await fixture();
  try {
    await assert.rejects(prepareContext({...start,resume:true},scope));
    const context=await prepareContext(start,scope);context.store.close();
    await assert.rejects(prepareContext({...start,resume:true},scope),/checkpoint_missing/);
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
