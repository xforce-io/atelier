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
  if (!value || Buffer.byteLength(value) > 256 || value.includes('\0')) throw new Error('tool_identity_invalid');
}

/** Caller must own the delivery's exclusive execution slot. This journal stores
 * transport identity, never infers equivalence between newly generated calls. */
export class ToolLedger {
  private tail: Promise<unknown> = Promise.resolve();
  constructor(private readonly directory: string, private readonly deliveryId: string) { identifier(deliveryId); }
  private serial<T>(action: () => Promise<T>): Promise<T> {
    const next = this.tail.then(action);
    this.tail = next.catch(() => {});
    return next;
  }
  private async ensure(): Promise<void> {
    await fs.mkdir(this.directory, { mode: 0o700 }).catch(error => { if (error.code !== 'EEXIST') throw error; });
    const stat = await fs.lstat(this.directory);
    if (!stat.isDirectory() || stat.isSymbolicLink()) throw new Error('tool_ledger_invalid');
    const parent = await fs.open(dirname(this.directory),'r');
    try { await parent.sync(); } finally { await parent.close(); }
  }
  private file(runId: string, callId: string): string { return join(this.directory, hash(stable([runId, callId])) + '.json'); }
  private async read(file: string): Promise<Record | undefined> {
    let stat;
    try { stat = await fs.lstat(file); } catch (error) { if ((error as NodeJS.ErrnoException).code === 'ENOENT') return undefined; throw error; }
    if (!stat.isFile() || stat.isSymbolicLink() || stat.size > 3 * limit) throw new Error('tool_ledger_corrupt');
    const record = JSON.parse(await fs.readFile(file, 'utf8')) as Record;
    if (record.version !== 1 || record.deliveryId !== this.deliveryId || !record.operation || !['pending','completed'].includes(record.state)) throw new Error('tool_ledger_corrupt');
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
  recover(forward: ForwardTool): Promise<RecoveredOperation[]> {
    return this.serial(async () => {
      await this.ensure();
      const recovered: RecoveredOperation[] = [];
      for (const filename of (await fs.readdir(this.directory)).filter(name => /^[a-f0-9]{64}\.json$/.test(name)).sort()) {
        const file = join(this.directory,filename);
        const record = await this.read(file);
        if (record?.state === 'pending') {
          const result = await this.forward(file,record,forward);
          recovered.push({operationId:record.operation.operationId,name:record.operation.name,result});
        }
      }
      return recovered;
    });
  }
}
