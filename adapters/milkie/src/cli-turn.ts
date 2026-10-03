import { closeSync, fsyncSync, openSync, writeFileSync, renameSync, unlinkSync } from 'node:fs';
import { promises as fs } from 'node:fs';
import { dirname, join } from 'node:path';
import { randomUUID } from 'node:crypto';
import { setTimeout as delay } from 'node:timers/promises';
import type { ExecutionClient, ToolSchema, ToolHandler } from '@freemanxu/milkie';
import { CliTools } from './cli-tools.js';
import { ToolLedger, type ForwardTool, type RecoveredOperation } from './tool-ledger.js';
import type { Scope, Terminal } from './channel.js';

export interface CliTurn {
  execution: ExecutionClient;
  nativeContextId: string;
  scope: Scope;
  /** Private directory within the bound native context. */
  journalDirectory: string;
  ledger: ToolLedger;
  tools: ToolSchema[];
  goal: string;
  input: string;
  skill: string;
  forward: ForwardTool;
  signal: AbortSignal;
}
interface Launch extends Scope { nativeContextId: string; }
interface Recovery { callId: string; output: string; operation?: RecoveredOperation; }
const identifier = (value: string) => /^[a-zA-Z0-9_-]{1,128}$/.test(value);
const failed = (code: string): Error => new Error(code);

/** Synchronous publication keeps the SDK's return -> run mapping boundary in
 * one host turn, before any handler can run. A crash before publication remains
 * an explicit missing binding, never permission to replay an unknown call. */
function save(path: string, value: unknown): void {
  const temporary = path + '.' + randomUUID() + '.tmp';
  const fd = openSync(temporary, 'wx', 0o600);
  try { writeFileSync(fd, JSON.stringify(value)); fsyncSync(fd); } finally { closeSync(fd); }
  try {
    renameSync(temporary, path);
    const fd = openSync(dirname(path), 'r');
    try { fsyncSync(fd); } finally { closeSync(fd); }
  } finally { try { unlinkSync(temporary); } catch (error) { if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error; } }
}
async function read(path: string): Promise<unknown> {
  const stat = await fs.lstat(path);
  if (!stat.isFile() || stat.isSymbolicLink() || stat.size > 1024 * 1024) throw failed('cli_journal_invalid');
  return JSON.parse(await fs.readFile(path, 'utf8')) as unknown;
}
function result(value: unknown): Awaited<ReturnType<ToolHandler>> {
  const encoded = JSON.stringify(value);
  if (encoded === undefined || Buffer.byteLength(encoded) > 65536) throw failed('cli_tool_result_too_large');
  // A business refusal is a failed tool call, not an SDK execution failure.
  if (value && typeof value === 'object' && (value as {ok?: unknown}).ok === false) {
    return { ok: false, code: 'rejected', message: encoded };
  }
  return { ok: true, output: encoded };
}

/** Own exactly one CLI execution; the core still owns the delivery and all
 * business decisions. Do not use wait()'s inferred unknown as a stopped fact. */
export async function executeCliTurn(turn: CliTurn): Promise<Terminal> {
  const { execution, nativeContextId, scope } = turn;
  if (![scope.taskId, scope.runId, scope.deliveryId, nativeContextId].every(identifier)) throw failed('cli_binding_invalid');
  const tools = new CliTools(turn.tools);
  const capabilities = execution.capabilities();
  if (!capabilities.supported || !capabilities.hostTools || !capabilities.resume || !capabilities.cancel || !capabilities.forwarding.includes('serial')) throw failed('cli_capabilities_missing');
  await fs.mkdir(turn.journalDirectory, { mode: 0o700 }).catch(error => { if (error.code !== 'EEXIST') throw error; });
  const stat = await fs.lstat(turn.journalDirectory);
  if (!stat.isDirectory() || stat.isSymbolicLink() || (stat.mode & 0o077) !== 0) throw failed('cli_journal_invalid');
  const recoveryPath = join(turn.journalDirectory, `${scope.deliveryId}.recovery.json`);
  let recoveries: Recovery[] = [];
  try {
    const value = await read(recoveryPath);
    if (!Array.isArray(value) || value.length > 100 || value.some(item => !item || typeof item.callId !== 'string' || !identifier(item.callId) || typeof item.output !== 'string' || item.output.length > 65536)) throw failed('cli_recovery_invalid');
    recoveries = value as Recovery[];
  } catch (error) { if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error; }
  const ensureActive = () => { if (turn.signal.aborted) throw failed('run_cancelled'); };
  const forward: ForwardTool = async (operation) => { ensureActive(); return turn.forward(operation, turn.signal); };
  ensureActive();
  // First recover the transport ledger, even if milkie managed to write a
  // rejection while the host was losing its connection to the core.
  // Include completed records: a crash after ledger completion but before the
  // next prompt must not erase the checked outcome. Core reauthorizes every
  // query, so cached data never leaks through revoked permissions.
  const transportRecovery = await turn.ledger.recover(forward, true);
  const callsToReconcile = new Map(execution.pendingToolCalls(nativeContextId).map(call => [call.callId, call]));
  for (const saved of recoveries) {
    const call = execution.toolCall(saved.callId);
    if (!call || call.contextId !== nativeContextId || !['pending', 'reconciled'].includes(call.status)) throw failed('cli_recovery_invalid');
    callsToReconcile.set(call.callId, call);
  }
  for (const call of callsToReconcile.values()) {
    ensureActive();
    if (!identifier(call.runId)) throw failed('cli_binding_invalid');
    const launch = await read(join(turn.journalDirectory, `${call.runId}.json`)) as Launch;
    if (launch.nativeContextId !== nativeContextId || launch.taskId !== scope.taskId || launch.deliveryId !== scope.deliveryId || !identifier(launch.runId)) throw failed('cli_recovery_binding_mismatch');
    const previous = execution.query(call.runId);
    if (!previous?.stopped || previous.status === 'starting' || previous.status === 'running') throw failed('cli_previous_run_not_stopped');
    {
      const operation = await turn.ledger.reconcileRecorded(launch.runId, call.callId, call.name, call.input, forward);
      // No host journal means no core request was issued. Never dispatch that
      // previously queued request for the first time during recovery.
      const output = JSON.stringify(operation ? { status: 'reconciled', ...operation } : { status: 'not_executed', reason: 'host_stopped_before_dispatch' });
      if (output.length > 65536) throw failed('cli_recovery_too_large');
      const saved = { callId: call.callId, output, ...(operation ? { operation } : {}) };
      recoveries = recoveries.filter(item => item.callId !== call.callId);
      recoveries.push(saved);
      if (recoveries.length > 100) throw failed('cli_recovery_too_large');
      save(recoveryPath, recoveries);
      if (call.status === 'pending') execution.reconcile(call.callId, saved.output);
    }
  }
  const recovered = new Map<string, RecoveredOperation>();
  for (const operation of [...transportRecovery, ...recoveries.flatMap(item => item.operation ? [item.operation] : [])]) recovered.set(operation.operationId, operation);
  const prompt = JSON.stringify({ goal: turn.goal, skill: turn.skill, workMessage: turn.input, reconciledOperations: [...recovered.values()], reconciledCalls: recoveries.map(({callId, output}) => ({callId, output})) });
  ensureActive();
  let runId: string | undefined;
  let uncertain = false;
  let exceeded = false;
  let calls = 0;
  let cancel: Promise<unknown> | undefined;
  const stop = () => { if (runId && !cancel) cancel = execution.cancel(runId).catch(() => undefined); };
  const onAbort = () => stop();
  turn.signal.addEventListener('abort', onAbort, { once: true });
  try {
    runId = execution.start(nativeContextId, prompt, { tools: tools.specs, forwarding: 'serial', timeoutMs: 15 * 60 * 1000 }, async call => {
      if (turn.signal.aborted || uncertain || exceeded) return { ok: false, code: 'rejected', message: 'Execution is stopping.' };
      if (++calls > 100) { exceeded = true; stop(); await cancel; return { ok: false, code: 'rejected', message: 'Tool call budget exhausted.' }; }
      if (call.runId !== runId || call.contextId !== nativeContextId) { uncertain = true; stop(); await cancel; throw failed('cli_call_binding_mismatch'); }
      const validity = tools.validate(call.name, call.input);
      if (validity !== 'allowed') return { ok: false, code: validity, message: validity === 'rejected' ? 'Tool is not authorized.' : 'Input does not match the full tool schema.' };
      try { return result(await turn.ledger.call(scope.runId, call.callId, call.name, call.input, forward)); }
      catch {
        uncertain = true;
        stop();
        // Stop before returning a tool failure to the model. The host ledger
        // remains pending when the core reply was lost; no replacement call.
        await cancel;
        throw failed('tool_result_uncertain');
      }
    });
    save(join(turn.journalDirectory, `${runId}.json`), { ...scope, nativeContextId });
    const deadline = Date.now() + 15 * 60 * 1000;
    while (true) {
      const record = execution.query(runId);
      if (!record) throw failed('cli_run_missing');
      if (record.stopped && record.status !== 'starting' && record.status !== 'running') {
        if (uncertain) throw failed('tool_result_uncertain');
        const stopReason: Terminal['stopReason'] = exceeded ? 'budget_exhausted' : record.status === 'succeeded' ? 'completed' : record.status === 'cancelled' ? 'cancelled' : record.status === 'timed_out' ? 'deadline' : 'failed';
        return { stopReason, ...(exceeded ? { stopCode: 'TOOL_CALL_BUDGET_EXCEEDED' } : record.code ? { stopCode: record.code } : {}), nativeStopReason: record.status, recoveredOperations: recovered.size };
      }
      if (record.status === 'unknown' || turn.signal.aborted || Date.now() >= deadline) {
        stop(); await cancel;
        const observed = execution.query(runId);
        if (!observed?.stopped) throw failed('cli_resources_unknown');
      }
      await delay(50);
    }
  } catch (error) {
    stop(); await cancel;
    throw error;
  } finally {
    turn.signal.removeEventListener('abort', onAbort);
  }
}
