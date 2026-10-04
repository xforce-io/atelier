import { createHash } from 'node:crypto';
import type { Readable, Writable } from 'node:stream';
import type { ForwardTool, ToolOperation } from './tool-ledger.js';

export const milkieCommit = 'a3c1af0e479c9307140e6335da0e55efcf8785cc';
const maxFrame = 2 * 1024 * 1024;
const maxBytes = 32 * 1024 * 1024;
const maxFrames = 1024;
export interface Scope { taskId: string; runId: string; deliveryId: string; }
interface Frame extends Scope { protocolVersion: 1; requestId: string; seq: number; kind: string; payload: unknown; }
interface Capabilities { milkieCommit: string; transport: string; skillDigest: string; builtinToolsDisabled: boolean; stableToolCallIds: boolean; nativeCheckpoint: boolean; privateTools: boolean; }
export interface Terminal { stopReason: 'completed'|'cancelled'|'deadline'|'budget_exhausted'|'failed'; stopCode?: string; nativeStopReason: string; recoveredOperations: number; }
const hash = (value: string|Buffer) => createHash('sha256').update(value).digest('hex');
const invalid = () => new Error('member_channel_protocol_invalid');
function canonical(value: unknown): string {
  if(Array.isArray(value)) return '['+value.map(canonical).join(',')+']';
  if(value!==null && typeof value==='object') return '{'+Object.keys(value).sort().map(k=>JSON.stringify(k)+':'+canonical((value as Record<string,unknown>)[k])).join(',')+'}';
  return JSON.stringify(value);
}
function object(value: unknown): Record<string,unknown> {
  if(!value || typeof value!=='object' || Array.isArray(value)) throw invalid();
  return value as Record<string,unknown>;
}
function capabilities(value: unknown): Capabilities {
  const p=object(value);
  if(Object.keys(p).sort().join(',')!=='builtinToolsDisabled,milkieCommit,nativeCheckpoint,privateTools,skillDigest,stableToolCallIds,transport'
    ||p.milkieCommit!==milkieCommit||!['api','agent-cli'].includes(p.transport as string)||p.builtinToolsDisabled!==true||p.stableToolCallIds!==true||p.nativeCheckpoint!==true||p.privateTools!==true
    ||typeof p.skillDigest!=='string'||!/^[a-f0-9]{64}$/.test(p.skillDigest)) throw invalid();
  return p as unknown as Capabilities;
}
async function* lines(stream: Readable): AsyncGenerator<unknown> {
  let partial=Buffer.alloc(0), bytes=0;
  for await(const part of stream) {
    const chunk=Buffer.isBuffer(part)?part:Buffer.from(part as string);
    bytes+=chunk.length;
    if(bytes>maxBytes) throw invalid();
    let offset=0;
    while(offset<chunk.length) {
      const newline=chunk.indexOf(10,offset);
      const end=newline<0?chunk.length:newline+1;
      if(partial.length+end-offset>maxFrame) throw invalid();
      partial=Buffer.concat([partial,chunk.subarray(offset,end)]);
      offset=end;
      if(newline<0) break;
      let value: unknown;
      try {value=JSON.parse(new TextDecoder('utf-8',{fatal:true}).decode(partial));} catch {throw invalid();}
      partial=Buffer.alloc(0);
      yield value;
    }
  }
  throw new Error('member_channel_closed');
}
class Receiver {
  readonly seen=new Map<number,string>();
  scope?: Scope;
  accept(value: unknown): {frame:Frame;duplicate:boolean} {
    const p=object(value);
    if(Object.keys(p).sort().join(',')!=='deliveryId,kind,payload,protocolVersion,requestId,runId,seq,taskId'
      ||p.protocolVersion!==1||!Number.isSafeInteger(p.seq)||(p.seq as number)<1||(p.seq as number)>maxFrames
      ||typeof p.kind!=='string'||typeof p.requestId!=='string'||!p.requestId||Buffer.byteLength(p.requestId)>256
      ||[p.taskId,p.runId,p.deliveryId].some(id=>typeof id!=='string'||!/^[a-zA-Z0-9_-]{1,128}$/.test(id))) throw invalid();
    const frame=p as unknown as Frame;
    if(this.scope && ['taskId','runId','deliveryId'].some(k=>frame[k as keyof Scope]!==this.scope![k as keyof Scope])) throw invalid();
    const digest=hash(canonical(frame));
    const previous=this.seen.get(frame.seq);
    if(previous!==undefined) {if(previous!==digest) throw invalid();return {frame,duplicate:true};}
    if(frame.seq!==this.seen.size+1) throw invalid();
    this.seen.set(frame.seq,digest);
    this.scope={taskId:frame.taskId,runId:frame.runId,deliveryId:frame.deliveryId};
    return {frame,duplicate:false};
  }
}

/** Only the adapter owns this pipe. No model-selected actor, socket endpoint,
 * management CLI, shell command, or database path is accepted by this class. */
export class AdapterChannel {
  private readonly receiver=new Receiver();
  private readonly controller=new AbortController();
  private readonly iterator: AsyncGenerator<unknown>;
  private sequence=0;
  private sentBytes=0;
  private tail:Promise<void>=Promise.resolve();
  private closed=false;
  private failure?: Error;
  private expected?: Capabilities;
  private readonly pending=new Map<string,{resolve:(value:unknown)=>void;reject:(error:Error)=>void}>();
  private readonly settled=new Map<string,string>();
  private constructor(private readonly input:Readable,private readonly output:Writable) {
    this.iterator=lines(input);
    this.output.on('error',()=>this.fail(new Error('member_channel_write_failed')));
  }
  get scope(): Scope {if(!this.receiver.scope) throw invalid();return {...this.receiver.scope};}
  get signal(): AbortSignal {return this.controller.signal;}
  get transport(): string {if(!this.expected) throw invalid();return this.expected.transport;}
  static async connect(input:Readable,output:Writable):Promise<AdapterChannel> {
    const channel=new AdapterChannel(input,output);
    try {
      const {frame}=await channel.next();
      if(frame.kind!=='hello'||frame.requestId!=='hello') throw invalid();
      channel.expected=capabilities(frame.payload);
      await channel.send('hello','ready',channel.expected);
      return channel;
    } catch(error) {channel.fail(error);throw error;}
  }
  private async next():Promise<{frame:Frame;duplicate:boolean}> {
    const next=await this.iterator.next();
    if(next.done) throw new Error('member_channel_closed');
    return this.receiver.accept(next.value);
  }
  private fail(error:unknown):void {
    if(this.closed) return;
    this.failure=error instanceof Error?error:new Error('member_channel_failed');
    this.controller.abort();
    for(const pending of this.pending.values()) pending.reject(this.failure);
    this.pending.clear();
    this.closed=true;
    this.input.destroy();
  }
  private send(requestId:string,kind:string,payload:unknown):Promise<void> {
    const next=this.tail.then(async()=>{
      if(this.closed) throw this.failure??new Error('member_channel_closed');
      if(++this.sequence>maxFrames) throw invalid();
      const frame:Frame={protocolVersion:1,...this.scope,requestId,seq:this.sequence,kind,payload};
      const data=JSON.stringify(frame)+'\n';
      const size=Buffer.byteLength(data);
      this.sentBytes+=size;
      if(size>maxFrame||this.sentBytes>maxBytes) throw invalid();
      await new Promise<void>((resolve,reject)=>{
        const timer=setTimeout(()=>reject(new Error('member_channel_write_timeout')),10_000);
        this.output.write(data,error=>{clearTimeout(timer);error?reject(error):resolve();});
      });
    });
    this.tail=next.catch(error=>this.fail(error));
    return next;
  }
  async start():Promise<Record<string,unknown>> {
    try {
      while(true) {
        const {frame,duplicate}=await this.next();
        if(duplicate && frame.kind==='hello') continue;
        if(frame.kind==='stop') {this.controller.abort();throw new Error('run_cancelled_before_start');}
        if(frame.kind!=='start'||frame.requestId!=='start') throw invalid();
        const start=object(frame.payload);
        if(typeof start.skill!=='string'||hash(start.skill)!==this.expected?.skillDigest) throw invalid();
        void this.receive().catch(error=>this.fail(error));
        return start;
      }
    } catch(error) {this.fail(error);throw error;}
  }
  private async receive():Promise<void> {
    while(!this.closed) {
      const {frame,duplicate}=await this.next();
      if(duplicate) continue;
      if(frame.kind==='stop') {
        this.controller.abort();
        for(const pending of this.pending.values()) pending.reject(new Error('run_cancelled'));
        this.pending.clear();
        continue;
      }
      if(frame.kind!=='tool.result') throw invalid();
      const pending=this.pending.get(frame.requestId);
      if(!pending) {
        if(this.signal.aborted) continue;
        if(this.settled.get(frame.requestId)!==hash(canonical(frame.payload))) throw invalid();
        continue;
      }
      this.pending.delete(frame.requestId);
      this.settled.set(frame.requestId,hash(canonical(frame.payload)));
      pending.resolve(frame.payload);
    }
  }
  readonly forward:ForwardTool=(operation:ToolOperation,signal?:AbortSignal):Promise<unknown>=>this.request(operation.operationId,'tool.request',operation,signal);
  readonly reconcile=(deliveryId:string,operation:ToolOperation,signal?:AbortSignal):Promise<unknown>=>this.request(operation.operationId,'tool.reconcile',{deliveryId,operation},signal);
  private request(operationId:string,kind:string,payload:unknown,signal?:AbortSignal):Promise<unknown> {
    if(this.closed||this.signal.aborted||signal?.aborted) return Promise.reject(this.failure??new Error('run_cancelled'));
    if(this.pending.has(operationId)) return Promise.reject(new Error('tool_operation_already_in_flight'));
    return new Promise<unknown>((resolve,reject)=>{
      const onAbort=()=>{this.pending.delete(operationId);reject(new Error('tool_request_aborted'));};
      const cleanup=()=>signal?.removeEventListener('abort',onAbort);
      this.pending.set(operationId,{resolve:value=>{cleanup();resolve(value);},reject:error=>{cleanup();reject(error);}});
      signal?.addEventListener('abort',onAbort,{once:true});
      void this.send(operationId,kind,payload).catch(error=>{
        cleanup();this.pending.delete(operationId);reject(error);
      });
    });
  }
  async finish(terminal:Terminal):Promise<void> {
    await this.send('terminal','terminal',terminal);
    this.closed=true;
    for(const pending of this.pending.values()) pending.reject(new Error('member_channel_finished'));
    this.pending.clear();
    this.input.destroy();
  }
}
