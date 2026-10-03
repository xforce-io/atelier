import { promises as fs } from 'node:fs';
import { isAbsolute, join } from 'node:path';
import { ExecutionClient } from '@freemanxu/milkie';
import { parseRunStart, type RunStart } from './api-process.js';
import { CliTools } from './cli-tools.js';
import { executeCliTurn } from './cli-turn.js';
import { ToolLedger } from './tool-ledger.js';
import { type AdapterChannel, type Scope } from './channel.js';

interface Start extends RunStart {
  connection: { runtime: 'pi'|'grok-cli'; model?: string; configDir: string; };
}
export function parseCliStart(value: unknown): Start {
  const p = parseRunStart(value);
  const c = p.connection;
  if (Object.keys(c).some(key => !['runtime','model','configDir'].includes(key)) || !['pi','grok-cli'].includes(c.runtime as string)
    || typeof c.configDir !== 'string' || !isAbsolute(c.configDir) || c.configDir.includes('\0')
    || (c.model !== undefined && (typeof c.model !== 'string' || !c.model.trim() || c.model.length > 256))) throw new Error('cli_start_invalid');
  new CliTools(p.tools); // Validate all constraints before touching a session.
  return p as unknown as Start;
}
async function privateDirectory(path: string): Promise<void> {
  const stat = await fs.lstat(path);
  if (!stat.isDirectory() || stat.isSymbolicLink() || (stat.mode & 0o077) !== 0) throw new Error('private_context_directory_invalid');
}
async function sync(path: string): Promise<void> {
  const fd = await fs.open(path, 'r');
  try { await fd.sync(); } finally { await fd.close(); }
}

/** Run inside the core-owned container. Paths are private control data, never
 * tool arguments. No ambient login discovery or CLI command construction. */
export async function prepareCliContext(start: Start, scope: Scope): Promise<{ execution: ExecutionClient; nativeContextId: string; journalDirectory: string }> {
  for (const path of [start.contextDirectory, start.ledgerDirectory, start.connection.configDir]) await privateDirectory(path);
  const roots = await Promise.all([start.contextDirectory, start.ledgerDirectory, start.connection.configDir].map(path => fs.realpath(path)));
  if (new Set(roots).size !== roots.length || roots.some((root, i) => roots.some((other, j) => i !== j && root.startsWith(other + '/')))) throw new Error('cli_storage_must_be_separate');
  const manifestPath = join(start.contextDirectory, 'binding.json');
  const sessionDir = join(start.contextDirectory, 'sessions');
  const dataDir = join(start.contextDirectory, 'sdk');
  const journalDirectory = join(start.contextDirectory, 'journal');
  const cwd = join(start.contextDirectory, 'cwd');
  const binding = { version: 1, transport: 'agent-cli', taskId: scope.taskId, workerId: start.workerId, contextId: start.contextId,
    configurationId: start.configurationId, purposeFamily: start.purposeFamily, runtime: start.connection.runtime, model: start.connection.model ?? null, configDir: roots[2] };
  let nativeContextId: string | undefined;
  if (start.resume) {
    const stat = await fs.lstat(manifestPath);
    if (!stat.isFile() || stat.isSymbolicLink() || stat.size > 4096) throw new Error('native_context_binding_invalid');
    const stored = JSON.parse(await fs.readFile(manifestPath, 'utf8')) as Record<string, unknown>;
    if (!stored || Object.keys(stored).length !== Object.keys(binding).length + 1 || Object.entries(binding).some(([key,value]) => stored[key] !== value)
      || typeof stored.nativeContextId !== 'string') throw new Error('native_context_binding_mismatch');
    nativeContextId = stored.nativeContextId;
    for (const directory of [sessionDir, dataDir, journalDirectory, cwd]) await privateDirectory(directory);
  } else {
    if ((await fs.readdir(start.contextDirectory)).length !== 0) throw new Error('native_context_already_exists');
    for (const directory of [sessionDir, dataDir, journalDirectory, cwd]) await fs.mkdir(directory, { mode: 0o700 });
  }
  const execution = new ExecutionClient({ dataDir, connection: { contractVersion: 2, legacyEnv: {}, fields: {
    transport: 'agent-cli', runtime: start.connection.runtime, ...(start.connection.model ? { model: start.connection.model } : {}),
  } } });
  if (nativeContextId) {
    const context = execution.getContext(nativeContextId);
    if (!context.hasExecuted) throw new Error('native_context_not_started');
    if (context.configDir !== roots[2] || context.sessionDir !== await fs.realpath(sessionDir) || context.cwd !== await fs.realpath(cwd)
      || context.connection.runtime !== start.connection.runtime || (context.connection.model ?? null) !== binding.model) throw new Error('native_context_binding_mismatch');
  } else {
    nativeContextId = execution.createContext(cwd, { configDir: start.connection.configDir, sessionDir }).contextId;
    const handle = await fs.open(manifestPath, 'wx', 0o600);
    try { await handle.writeFile(JSON.stringify({ ...binding, nativeContextId })); await handle.sync(); } finally { await handle.close(); }
    await sync(start.contextDirectory);
  }
  return { execution, nativeContextId, journalDirectory };
}
export async function runCliProcess(channel: AdapterChannel): Promise<void> {
  const start = parseCliStart(await channel.start());
  if (channel.signal.aborted) throw new Error('run_cancelled_before_start');
  const context = await prepareCliContext(start, channel.scope);
  // Each new delivery has its own transport ledger, while native dialogue is
  // retained across deliveries. A retry reopens this same delivery directory.
  const ledgerDirectory = join(start.ledgerDirectory, channel.scope.deliveryId);
  const terminal = await executeCliTurn({ ...context, scope: channel.scope, ledger: new ToolLedger(ledgerDirectory, channel.scope.deliveryId),
    tools: start.tools, goal: start.goal, input: start.input, skill: start.skill, forward: channel.forward, signal: channel.signal });
  await channel.finish(terminal);
}
