import { createHash, randomUUID } from 'node:crypto';
import { promises as fs } from 'node:fs';
import { dirname, join } from 'node:path';

export interface ToolOperation {
  operationId: string;
  originatingRunId: string;
  toolCallId: string;
  name: string;
  input: unknown;
}
interface Record {
  version: 1;
  deliveryId: string;
  operation: ToolOperation;
  fingerprint: string;
  state: 'pending' | 'completed';
  result?: unknown;
}
export interface RecoveredOperation { operationId: string; name: string; result: unknown; }
export type ForwardTool = (operation: ToolOperation, signal?: AbortSignal) => Promise<unknown>;
const limit = 256 * 1024;
function stable(value: unknown): string {
  if (value === null || typeof value === 'string' || typeof value === 'boolean') return JSON.stringify(value);
  if (typeof value === 'number' && Number.isFinite(value)) return JSON.stringify(value);
  if (Array.isArray(value)) return '[' + value.map(stable).join(',') + ']';
  if (typeof value === 'object' && value && Object.getPrototypeOf(value) === Object.prototype) {
    return '{' + Object.keys(value).sort().map(key => JSON.stringify(key) + ':' + stable((value as { [key: string]: unknown })[key])).join(',') + '}';
  }
  throw new Error('tool_payload_invalid');
}
const hash = (text: string) => createHash('sha256').update(text).digest('hex');
function identifier(value: string): void {
  if (typeof value !== 'string' || !value || Buffer.byteLength(value) > 256 || value.includes('\0')) throw new Error('tool_identity_invalid');
}

const recordName = /^[a-f0-9]{64}\.json$/;
const temporaryName = /^[a-f0-9]{64}\.json\.[a-f0-9-]{36}\.tmp$/;
function deliveryDirectoryId(value: string): void {
  if (typeof value !== 'string' || !/^[a-zA-Z0-9_-]{1,128}$/.test(value)) throw new Error('tool_delivery_invalid');
}
async function syncDirectory(path: string): Promise<void> {
  const fd = await fs.open(path, 'r');
  try { await fd.sync(); } finally { await fd.close(); }
}

/** Caller must own the delivery's exclusive execution slot. This journal stores
 * transport identity, never infers equivalence between newly generated calls. */
export class ToolLedger {
  private tail: Promise<unknown> = Promise.resolve();
  constructor(private readonly directory: string, private readonly deliveryId: string) { identifier(deliveryId); }
  /** Native dialogue spans deliveries; operation identity does not. The core
   * owns an exclusive context slot while opening. Old development layouts are
   * moved without replaying calls; an unresolved foreign delivery blocks all
   * new model work until the core can reconcile it under its original scope. */
  static async openDelivery(root: string, deliveryId: string): Promise<ToolLedger> {
    deliveryDirectoryId(deliveryId);
    const stat = await fs.lstat(root);
    if (!stat.isDirectory() || stat.isSymbolicLink() || (stat.mode & 0o077) !== 0) throw new Error('tool_ledger_invalid');
    const legacy: { file: string; name: string; record: Record }[] = [];
    const check = (record: Record) => {
      if (record.deliveryId !== deliveryId && record.state === 'pending') throw new Error('tool_foreign_delivery_unreconciled');
    };
    // Validate everything before migrating anything. Temporary files precede
    // the atomic pending record and therefore have never authorized forwarding.
    for (const entry of await fs.readdir(root, { withFileTypes: true })) {
      const file = join(root, entry.name);
      if (entry.isFile() && temporaryName.test(entry.name)) continue;
      if (entry.isFile() && recordName.test(entry.name)) {
        const metadata = await fs.lstat(file);
        if (metadata.size > 3 * limit) throw new Error('tool_ledger_corrupt');
        const raw = JSON.parse(await fs.readFile(file, 'utf8')) as Record;
        deliveryDirectoryId(raw.deliveryId);
        const record = await new ToolLedger(root, raw.deliveryId).read(file);
        if (!record) throw new Error('tool_ledger_corrupt');
        check(record); legacy.push({file,name:entry.name,record});
      } else if (entry.isDirectory()) {
        deliveryDirectoryId(entry.name);
        const metadata = await fs.lstat(file);
        if ((metadata.mode & 0o077) !== 0) throw new Error('tool_ledger_invalid');
        const ledger = new ToolLedger(file, entry.name);
        for (const item of await fs.readdir(file, { withFileTypes: true })) {
          if (item.isFile() && temporaryName.test(item.name)) continue;
          if (!item.isFile() || !recordName.test(item.name)) throw new Error('tool_ledger_corrupt');
          const record = await ledger.read(join(file,item.name));
          if (!record) throw new Error('tool_ledger_corrupt');
          check(record);
        }
      } else throw new Error('tool_ledger_corrupt');
    }
    for (const {file,name,record} of legacy) {
      const ledger = new ToolLedger(join(root,record.deliveryId),record.deliveryId);
      await ledger.ensure();
      const target = join(ledger.directory,name);
      try { await fs.link(file,target); }
      catch (error) {
        if ((error as NodeJS.ErrnoException).code !== 'EEXIST') throw error;
        const saved = await ledger.read(target);
        if (!saved || stable(saved) !== stable(record)) throw new Error('tool_ledger_migration_conflict');
      }
      // Link + fsync + unlink also tolerates a crash with both names present.
      // Unlike rename, it never overwrites a conflicting durable operation.
      await syncDirectory(ledger.directory);
      await fs.unlink(file);
      await syncDirectory(root);
    }
    const ledger = new ToolLedger(join(root,deliveryId),deliveryId);
    await ledger.ensure();
    return ledger;
  }
  private serial<T>(action: () => Promise<T>): Promise<T> {
    const next = this.tail.then(action);
    this.tail = next.catch(() => {});
    return next;
  }
  private async ensure(): Promise<void> {
    await fs.mkdir(this.directory, { mode: 0o700 }).catch(error => { if (error.code !== 'EEXIST') throw error; });
    const stat = await fs.lstat(this.directory);
    if (!stat.isDirectory() || stat.isSymbolicLink() || (stat.mode & 0o077) !== 0) throw new Error('tool_ledger_invalid');
    const parent = await fs.open(dirname(this.directory),'r');
    try { await parent.sync(); } finally { await parent.close(); }
  }
  private file(runId: string, callId: string): string { return join(this.directory, hash(stable([runId, callId])) + '.json'); }
  private async read(file: string): Promise<Record | undefined> {
    let stat;
    try { stat = await fs.lstat(file); } catch (error) { if ((error as NodeJS.ErrnoException).code === 'ENOENT') return undefined; throw error; }
    if (!stat.isFile() || stat.isSymbolicLink() || stat.size > 3 * limit) throw new Error('tool_ledger_corrupt');
    const record = JSON.parse(await fs.readFile(file, 'utf8')) as Record;
    if (!record || record.version !== 1 || record.deliveryId !== this.deliveryId || !record.operation || !['pending','completed'].includes(record.state)
      || (record.state === 'completed' && !Object.hasOwn(record,'result'))) throw new Error('tool_ledger_corrupt');
    for (const id of [record.operation.operationId, record.operation.originatingRunId, record.operation.toolCallId, record.operation.name]) identifier(id);
    if (this.file(record.operation.originatingRunId, record.operation.toolCallId) !== file || hash(stable([record.operation.name,record.operation.input])) !== record.fingerprint) throw new Error('tool_ledger_corrupt');
    return record;
  }
  private async write(file: string, record: Record): Promise<void> {
    const temporary = file + '.' + randomUUID() + '.tmp';
    const handle = await fs.open(temporary, 'wx', 0o600);
    try {
      await handle.writeFile(JSON.stringify(record));
      await handle.sync();
      await handle.close();
      await fs.rename(temporary, file);
      const directory = await fs.open(this.directory, 'r');
      try { await directory.sync(); } finally { await directory.close(); }
    } finally {
      await handle.close().catch(() => {});
      await fs.unlink(temporary).catch(error => { if (error.code !== 'ENOENT') throw error; });
    }
  }
  private async forward(file: string, record: Record, forward: ForwardTool): Promise<unknown> {
    const result = await forward(structuredClone(record.operation));
    const serialized = stable(result);
    if (Buffer.byteLength(serialized) > limit) throw new Error('tool_result_too_large');
    record.state = 'completed';
    record.result = JSON.parse(serialized) as unknown;
    await this.write(file,record);
    return record.result;
  }
  call(runId: string, callId: string, name: string, input: unknown, forward: ForwardTool): Promise<unknown> {
    return this.serial(async () => {
      for (const id of [runId,callId,name]) identifier(id);
      const payload = stable([name,input]);
      if (Buffer.byteLength(payload) > limit) throw new Error('tool_input_too_large');
      await this.ensure();
      const file = this.file(runId,callId);
      let record = await this.read(file);
      if (record) {
        if (record.fingerprint !== hash(payload)) throw new Error('tool_call_identity_conflict');
        // Core always rechecks current authorization, even for saved results.
        return this.forward(file,record,forward);
      }
      record = {version:1,deliveryId:this.deliveryId,operation:{operationId:randomUUID(),originatingRunId:runId,toolCallId:callId,name,input:JSON.parse(stable(input)) as unknown},fingerprint:hash(payload),state:'pending'};
      await this.write(file,record);
      return this.forward(file,record,forward);
    });
  }
  recover(forward: ForwardTool, includeCompleted = false): Promise<RecoveredOperation[]> {
    return this.serial(async () => {
      await this.ensure();
      const recovered: RecoveredOperation[] = [];
      for (const filename of (await fs.readdir(this.directory)).filter(name => /^[a-f0-9]{64}\.json$/.test(name)).sort()) {
        const file = join(this.directory,filename);
        const record = await this.read(file);
        if (record && (includeCompleted || record.state === 'pending')) {
          const result = await this.forward(file,record,forward);
          recovered.push({operationId:record.operation.operationId,name:record.operation.name,result});
        }
      }
      return recovered;
    });
  }
  /** A CLI may have persisted a call before handing it to this host. Recovery
   * must never turn such a queued call into a new business operation. */
  reconcileRecorded(runId: string, callId: string, name: string, input: unknown, forward: ForwardTool): Promise<RecoveredOperation | undefined> {
    return this.serial(async () => {
      identifier(runId); identifier(callId);
      await this.ensure();
      const file = this.file(runId, callId);
      const record = await this.read(file);
      if (!record) return undefined;
      if (record.fingerprint !== hash(stable([name,input]))) throw new Error('tool_call_identity_conflict');
      const result = await this.forward(file, record, forward);
      return { operationId: record.operation.operationId, name: record.operation.name, result };
    });
  }
}
