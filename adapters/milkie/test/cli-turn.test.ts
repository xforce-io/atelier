import assert from 'node:assert/strict';
import { test } from 'node:test';
import { mkdtemp, mkdir, chmod, symlink, rm, readFile, readdir, writeFile } from 'node:fs/promises';
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
