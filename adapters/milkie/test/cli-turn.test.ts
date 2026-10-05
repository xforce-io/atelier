import assert from 'node:assert/strict';
import { test } from 'node:test';
import { mkdtemp, mkdir, chmod, symlink, rm, readFile, readdir, writeFile, stat } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { randomUUID } from 'node:crypto';
import { ExecutionClient } from '@freemanxu/milkie';
import { executeCliTurn, type CliTurn } from '../src/cli-turn.js';
import { ToolLedger, type ToolOperation } from '../src/tool-ledger.js';

async function fixture() {
  const root = await mkdtemp(join(tmpdir(), 'atelier-cli-'));
  for (const name of ['bin', 'cwd', 'config', 'sessions', 'journal', 'ledger']) await mkdir(join(root, name), { mode: 0o700 });
  const script = resolve('test/fixtures/pi.cjs');
  await chmod(script, 0o755); await symlink(script, join(root, 'bin', 'pi'));
  await writeFile(join(root, 'config', 'auth.json'), '{"fixture":true}', { mode: 0o600 });
  const execution = new ExecutionClient({ dataDir: join(root, 'data'), connection: { contractVersion: 1, fields: { transport: 'agent-cli', runtime: 'pi' } }, env: { PATH: `${join(root, 'bin')}:${process.env.PATH}` } });
  const context = execution.createContext(join(root, 'cwd'), { configDir: join(root, 'config'), sessionDir: join(root, 'sessions') });
  const controller = new AbortController();
  const operations: ToolOperation[] = [];
  const options: CliTurn = {
    execution, nativeContextId: context.contextId, scope: { taskId: 'task', runId: 'run-one', deliveryId: 'delivery' },
    journalDirectory: join(root, 'journal'), ledger: new ToolLedger(join(root, 'ledger'), 'delivery'),
    tools: [{ name: 'message_send', description: '发送工作说明', inputSchema: {
      type: 'object', properties: { kind: { enum: ['work.note', 'work.question'] }, body: { type: 'string', minLength: 1, maxLength: 16 } }, required: ['kind', 'body'], additionalProperties: false,
    } }], goal: '处理投递', skill: '仅使用当前获准工具', input: JSON.stringify({ calls: [] }),
    forward: async operation => { operations.push(operation); return { ok: true, reference: 'message-one' }; }, signal: controller.signal,
  };
  return { root, options, operations, controller, cleanup: () => rm(root, { recursive: true, force: true }) };
}
const call = { name: 'message_send', input: { kind: 'work.note', body: '收到' } };

function boundedResult(bytes: number, character = 'x') {
  const overhead = Buffer.byteLength(JSON.stringify({ok:true,content:''}));
  const content = character.repeat(Math.floor((bytes-overhead)/Buffer.byteLength(character)));
  return {ok:true,content};
}

test('CLI rejects missing model-iteration capability before creating a journal or native Run',async()=>{
  const f=await fixture();
  try {
    const capabilities=f.options.execution.capabilities();
    f.options.execution.capabilities=()=>({...capabilities,modelIterations:false});
    await assert.rejects(executeCliTurn(f.options),/cli_capabilities_missing/);
    assert.deepEqual(await readdir(join(f.root,'journal')),[]);assert.equal(f.operations.length,0);
    await assert.rejects(readFile(join(f.root,'cwd','last-args.json')),/ENOENT/);
  }finally{await f.cleanup();}
});

test('SDK iteration exhaustion maps to budget_exhausted and the next Run retains its session',async()=>{
  const f=await fixture();
  try {
    const exhausted=await executeCliTurn({...f.options,input:JSON.stringify({iterationLoop:true})});
    assert.equal(exhausted.stopReason,'budget_exhausted');assert.equal(exhausted.stopCode,'iteration_budget_exhausted');
    assert.deepEqual(JSON.parse(await readFile(join(f.root,'cwd','fixture-iterations.json'),'utf8')),{limit:50,requests:50});
    assert.equal(f.operations.length,0);
    const session=f.options.execution.getContext(f.options.nativeContextId).nativeSessionId;
    assert.equal((await executeCliTurn({...f.options,scope:{...f.options.scope,runId:'after-iteration-budget'}})).stopReason,'completed');
    assert.equal(f.options.execution.getContext(f.options.nativeContextId).nativeSessionId,session);
  }finally{await f.cleanup();}
});

test('CLI delivers legal 80 KiB and maximum UTF-8 results through the fixed SDK',async()=>{
  const f=await fixture();
  try {
    const effects=new Map<string,ReturnType<typeof boundedResult>>();
    for(const [index,value] of [boundedResult(80*1024),boundedResult(256*1024),boundedResult(256*1024,'中')].entries()) {
      f.options.forward=async operation=>{f.operations.push(operation);if(!effects.has(operation.operationId))effects.set(operation.operationId,value);return effects.get(operation.operationId);};
      const terminal=await executeCliTurn({...f.options,scope:{...f.options.scope,runId:`large-${index}`},input:JSON.stringify({calls:[call]})});
      assert.equal(terminal.stopReason,'completed');
      const replies=JSON.parse(await readFile(join(f.root,'cwd','last-results.json'),'utf8'));
      assert.equal(replies[0].ok,true);assert.deepEqual(JSON.parse(replies[0].output),value);
      const calls=await readdir(join(f.root,'data','calls'));
      const records=await Promise.all(calls.filter(name=>name.endsWith('.json')).map(async name=>JSON.parse(await readFile(join(f.root,'data','calls',name),'utf8'))));
      assert.ok(records.some(record=>record.status==='succeeded'&&record.output&&JSON.stringify(JSON.parse(record.output))===replies[0].output));
      const runs=await readdir(join(f.root,'data','runs'));
      const recordsRun=await Promise.all(runs.filter(name=>/^[a-f0-9-]+\.json$/.test(name)).map(async name=>JSON.parse(await readFile(join(f.root,'data','runs',name),'utf8'))));
      assert.ok(recordsRun.every(record=>record.iterationBudget?.limit===50&&record.stopped));
    }
    assert.equal(effects.size,3);assert.equal(new Set(f.operations.map(operation=>operation.operationId)).size,3);
  }finally{await f.cleanup();}
});

test('a full-size lost result reconciles without adding metadata to the SDK output limit',async()=>{
  const f=await fixture();
  try {
    const value=boundedResult(256*1024);assert.equal(Buffer.byteLength(JSON.stringify(value)),256*1024);
    const effects=new Map<string,unknown>();let lost=true;
    f.options.forward=async operation=>{
      f.operations.push(operation);effects.set(operation.operationId,value);
      if(lost){lost=false;throw new Error('lost full-size reply');}return effects.get(operation.operationId);
    };
    await assert.rejects(executeCliTurn({...f.options,input:JSON.stringify({calls:[call]})}),/tool_result_uncertain/);
    const pending=f.options.execution.pendingToolCalls(f.options.nativeContextId);assert.equal(pending.length,1);
    const session=f.options.execution.getContext(f.options.nativeContextId).nativeSessionId;
    const terminal=await executeCliTurn({...f.options,scope:{...f.options.scope,runId:'full-recovery'}});
    assert.equal(terminal.stopReason,'completed');assert.equal(terminal.recoveredOperations,1);assert.equal(effects.size,1);
    const sdk=f.options.execution.toolCall(pending[0]!.callId)!;assert.equal(sdk.status,'reconciled');assert.deepEqual(JSON.parse(sdk.output!),value);assert.equal(Buffer.byteLength(sdk.output!),256*1024);
    assert.equal(f.options.execution.getContext(f.options.nativeContextId).nativeSessionId,session);
    const prompt=JSON.parse(await readFile(join(f.root,'cwd','last-prompt.json'),'utf8'));
    assert.deepEqual(prompt.reconciledOperations[0].result,value);
    assert.equal(prompt.reconciledCalls[0].operationId,prompt.reconciledOperations[0].operationId);
    assert.equal(prompt.reconciledCalls[0].output,undefined);
    assert.equal((await executeCliTurn({...f.options,scope:{...f.options.scope,runId:'full-recovery-again'}})).stopReason,'completed');
    assert.equal(effects.size,1);
  }finally{await f.cleanup();}
});

test('multiple full results reopen a recovery journal larger than 1 MiB without duplicate effects',async()=>{
  const f=await fixture();
  try {
    const value=boundedResult(256*1024);const effects=new Map<string,unknown>();
    f.options.forward=async operation=>{if(!effects.has(operation.operationId))effects.set(operation.operationId,value);return effects.get(operation.operationId);};
    await executeCliTurn({...f.options,input:JSON.stringify({calls:[call,call]})});
    assert.equal(effects.size,2);
    // Inject lost SDK publication after core/host journal completion; processes are stopped.
    for(const name of await readdir(join(f.root,'data','calls'))) {
      if(!name.endsWith('.json'))continue;
      const path=join(f.root,'data','calls',name);const record=JSON.parse(await readFile(path,'utf8'));
      assert.equal(record.status,'succeeded');record.status='pending';delete record.output;await writeFile(path,JSON.stringify(record));
    }
    const next=await executeCliTurn({...f.options,scope:{...f.options.scope,runId:'two-large-recovery'}});
    assert.equal(next.stopReason,'completed');assert.equal(next.recoveredOperations,2);
    assert.ok((await stat(join(f.root,'journal','delivery.recovery.json'))).size>1024*1024);
    assert.equal((await executeCliTurn({...f.options,scope:{...f.options.scope,runId:'two-large-reopen'}})).stopReason,'completed');
    assert.equal(effects.size,2);assert.equal(f.options.execution.pendingToolCalls(f.options.nativeContextId).length,0);
  }finally{await f.cleanup();}
});

test('real SDK maps read_file to a non-native tool and reconciles canonical calls after revocation', async () => {
  const f = await fixture();
  try {
    f.options.tools.push({name:'read_file',description:'读取候选文件',inputSchema:{type:'object',properties:{path:{type:'string',minLength:1}},required:['path'],additionalProperties:false}});
    const read = {name:'atelier_read_file',input:{path:'index.html'}};
    f.options.input = JSON.stringify({calls:[{...read,name:'read_file'}, {...read,input:{path:''}}, read]});
    assert.equal((await executeCliTurn(f.options)).stopReason,'completed');
    assert.equal(f.operations.length,1);
    assert.equal(f.operations[0]!.name,'read_file');
    const responses = JSON.parse(await readFile(join(f.root,'cwd','last-results.json'),'utf8'));
    assert.equal(responses[0].code,'rejected');
    assert.equal(responses[1].code,'invalid_input');
    assert.equal(responses[2].ok,true);
    const prompt = JSON.parse(await readFile(join(f.root,'cwd','last-prompt.json'),'utf8'));
    assert.equal(prompt.cliToolNames.read_file,'atelier_read_file');
    let revoked = false;
    f.options.forward = async operation => {
      assert.equal(operation.name,'read_file');
      if (!revoked && operation.originatingRunId === 'run-two') throw new Error('lost read reply');
      return revoked ? {ok:false,error:'forbidden'} : {ok:true,content:'old-authorized-data'};
    };
    await assert.rejects(executeCliTurn({...f.options,scope:{...f.options.scope,runId:'run-two'},input:JSON.stringify({calls:[read]})}),/tool_result_uncertain/);
    revoked = true;
    const terminal = await executeCliTurn({...f.options,scope:{...f.options.scope,runId:'run-three'},tools:[f.options.tools[0]!],input:JSON.stringify({calls:[read]})});
    assert.equal(terminal.stopReason,'completed');
    const after = JSON.parse(await readFile(join(f.root,'cwd','last-prompt.json'),'utf8'));
    assert.ok(after.reconciledOperations.every((op:{result:{error:string}})=>op.result.error==='forbidden'));
    assert.equal(JSON.stringify(after).includes('old-authorized-data'),false);
    assert.equal(JSON.parse(await readFile(join(f.root,'cwd','last-results.json'),'utf8'))[0].code,'rejected');
    assert.equal(f.options.execution.pendingToolCalls(f.options.nativeContextId).length,0);
  } finally { await f.cleanup(); }
});

test('real SDK forwards only valid authorized CLI calls and retains native session on next turn', async () => {
  const f = await fixture();
  try {
    f.options.input = JSON.stringify({ calls: [call, { ...call, input: { kind: 'invented', body: 'x' } }, { name: 'bash', input: {} }] });
    assert.equal((await executeCliTurn(f.options)).stopReason, 'completed');
    assert.equal(f.operations.length, 1);
    assert.equal(f.operations[0]!.originatingRunId, 'run-one');
    assert.ok(f.operations[0]!.toolCallId);
    const responses = JSON.parse(await readFile(join(f.root, 'cwd', 'last-results.json'), 'utf8'));
    assert.equal(responses[0].ok, true);
    assert.equal(responses[1].code, 'invalid_input');
    assert.equal(responses[2].code, 'rejected');
    const args = JSON.parse(await readFile(join(f.root, 'cwd', 'last-args.json'), 'utf8'));
    assert.ok(args.includes('--no-builtin-tools'));
    const session = f.options.execution.getContext(f.options.nativeContextId).nativeSessionId;
    assert.equal((await executeCliTurn({ ...f.options, scope: { ...f.options.scope, runId: 'run-two' }, input: JSON.stringify({ calls: [] }) })).stopReason, 'completed');
    assert.equal(f.options.execution.getContext(f.options.nativeContextId).nativeSessionId, session);
  } finally { await f.cleanup(); }
});

test('lost core reply stops the CLI; restart reconciles the same operation before any new call', async () => {
  const f = await fixture();
  try {
    const effects = new Map<string, unknown>();
    let lost = true;
    f.options.forward = async operation => {
      f.operations.push(operation);
      if (!effects.has(operation.operationId)) effects.set(operation.operationId, { ok: true, reference: 'once' });
      if (lost) { lost = false; throw new Error('reply lost after commit'); }
      return effects.get(operation.operationId);
    };
    f.options.input = JSON.stringify({ calls: [call, call] });
    await assert.rejects(executeCliTurn(f.options), /tool_result_uncertain/);
    assert.equal(effects.size, 1);
    const next = { ...f.options, scope: { ...f.options.scope, runId: 'run-two' }, ledger: new ToolLedger(join(f.root, 'ledger'), 'delivery'), input: JSON.stringify({ calls: [] }) };
    const terminal = await executeCliTurn(next);
    assert.equal(terminal.stopReason, 'completed');
    assert.equal(terminal.recoveredOperations, 1);
    assert.equal(effects.size, 1);
    assert.equal(new Set(f.operations.map(item => item.operationId)).size, 1);
    const prompt = JSON.parse(await readFile(join(f.root, 'cwd', 'last-prompt.json'), 'utf8'));
    assert.equal(prompt.reconciledOperations[0].result.reference, 'once');
    assert.equal(f.options.execution.pendingToolCalls(f.options.nativeContextId).length, 0);
  } finally { await f.cleanup(); }
});

test('recovery of an SDK queued call without host ledger never creates a business operation', async () => {
  const f = await fixture();
  try {
    await executeCliTurn(f.options);
    const launch = (await readdir(join(f.root, 'journal'))).find(name => !name.endsWith('.recovery.json'))!;
    const callId = randomUUID();
    const nativeRunId = launch.slice(0, -5);
    await writeFile(join(f.root, 'data', 'calls', `${callId}.json`), JSON.stringify({ version: 1, callId, ...call, runId: nativeRunId, contextId: f.options.nativeContextId, status: 'pending' }));
    await executeCliTurn({ ...f.options, scope: { ...f.options.scope, runId: 'run-two' } });
    assert.equal(f.operations.length, 0);
    assert.equal(f.options.execution.toolCall(callId)?.status, 'reconciled');
    const prompt = JSON.parse(await readFile(join(f.root, 'cwd', 'last-prompt.json'), 'utf8'));
    assert.equal(JSON.parse(prompt.reconciledCalls[0].output).status, 'not_executed');
  } finally { await f.cleanup(); }
});

test('cancel before start creates no native Run or core operation', async () => {
  const f = await fixture();
  try {
    f.controller.abort();
    await assert.rejects(executeCliTurn(f.options), /run_cancelled/);
    assert.equal(f.operations.length, 0);
    assert.equal((await readdir(join(f.root, 'journal'))).length, 0);
  } finally { await f.cleanup(); }
});

test('recovery rechecks current authority after a later restart instead of reusing saved output', async () => {
  const f = await fixture();
  try {
    let lost = true;
    let allowed = true;
    f.options.forward = async () => {
      if (lost) { lost = false; throw new Error('reply lost'); }
      return allowed ? { ok: true, privateDetail: 'old-authorized-data' } : { ok: false, error: 'forbidden' };
    };
    await assert.rejects(executeCliTurn({...f.options,input:JSON.stringify({calls:[call]})}), /tool_result_uncertain/);
    await executeCliTurn({...f.options,scope:{...f.options.scope,runId:'run-two'}});
    allowed = false;
    await executeCliTurn({...f.options,scope:{...f.options.scope,runId:'run-three'}});
    const prompt = await readFile(join(f.root,'cwd','last-prompt.json'),'utf8');
    assert.equal(prompt.includes('old-authorized-data'),false);
    assert.equal(JSON.parse(prompt).reconciledOperations[0].result.error,'forbidden');
  } finally { await f.cleanup(); }
});

test('foreign pending delivery prevents a new native execution and is not reassigned', async () => {
  const f = await fixture();
  try {
    await executeCliTurn(f.options);
    const launch = (await readdir(join(f.root,'journal'))).find(name => !name.endsWith('.recovery.json'))!;
    const callId = randomUUID();
    await writeFile(join(f.root,'data','calls',`${callId}.json`),JSON.stringify({version:1,callId,...call,runId:launch.slice(0,-5),contextId:f.options.nativeContextId,status:'pending'}));
    await assert.rejects(executeCliTurn({...f.options,scope:{...f.options.scope,runId:'run-two',deliveryId:'other-delivery'}}), /binding_mismatch/);
    assert.equal(f.operations.length,0);
    assert.equal(f.options.execution.toolCall(callId)?.status,'pending');
  } finally { await f.cleanup(); }
});

test('a different delivery reconciles old SDK and host journals read-only before continuing native history',async()=>{
  const f=await fixture();
  try {
    const ledgerRoot=join(f.root,'ledger');
    f.options.ledger=await ToolLedger.openDelivery(ledgerRoot,'delivery');
    const effects=new Map<string,unknown>();
    f.options.forward=async operation=>{f.operations.push(operation);effects.set(operation.operationId,{ok:true,reference:'committed-once'});throw new Error('lost reply');};
    f.options.input=JSON.stringify({calls:[call]});
    await assert.rejects(executeCliTurn(f.options),/tool_result_uncertain/);
    const session=f.options.execution.getContext(f.options.nativeContextId).nativeSessionId;
    let queries=0;
    const reconcile=async(delivery:string,operation:ToolOperation)=>{queries++;assert.equal(delivery,'delivery');assert.deepEqual(operation,f.operations[0]);return effects.get(operation.operationId);};
    const ledger=await ToolLedger.openDelivery(ledgerRoot,'different-delivery',reconcile);
    assert.equal(ledger.foreignRecovery.length,1);
    const terminal=await executeCliTurn({...f.options,scope:{...f.options.scope,runId:'new-run',deliveryId:'different-delivery'},ledger,reconcile,input:JSON.stringify({calls:[]}),forward:async()=>{throw new Error('no new call expected');}});
    assert.equal(terminal.stopReason,'completed');assert.equal(terminal.recoveredOperations,1);
    assert.equal(f.operations.length,1);assert.ok(queries>=1);assert.equal(effects.size,1);
    assert.equal(f.options.execution.getContext(f.options.nativeContextId).nativeSessionId,session);
    assert.equal(f.options.execution.pendingToolCalls(f.options.nativeContextId).length,0);
    const prompt=JSON.parse(await readFile(join(f.root,'cwd','last-prompt.json'),'utf8'));
    assert.equal(prompt.reconciledOperations[0].result.reference,'committed-once');
    const oldFile=(await readdir(join(ledgerRoot,'delivery')))[0]!;
    assert.equal(JSON.parse(await readFile(join(ledgerRoot,'delivery',oldFile),'utf8')).state,'completed');
  }finally{await f.cleanup();}
});
