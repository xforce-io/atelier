import { promises as fs } from 'node:fs';
import { isAbsolute, join } from 'node:path';
import { JsonlEventStore, SQLiteStore, assembleApiGateway, resolveAndParseConnection, type AgentCheckpoint, type ToolSchema } from '@freemanxu/milkie';
import { executeApiTurn } from './api-turn.js';
import { ToolLedger } from './tool-ledger.js';
import { AdapterChannel, type Scope, type Terminal } from './channel.js';

export interface RunStart {
  workerId:string; contextId:string; configurationId:string; purposeFamily:'coordinate'|'execute'|'verify';
  contextDirectory:string; ledgerDirectory:string; resume:boolean;
  goal:string; input:string; skill:string; tools:ToolSchema[];
}
interface Start extends RunStart { connection:{protocol:'anthropic-messages'|'openai-chat-completions';model:string;baseUrl?:string;apiKey:string}; }
function invalid():Error {return new Error('adapter_start_invalid');}
function record(value:unknown):Record<string,unknown> {
  if(!value||typeof value!=='object'||Array.isArray(value))throw invalid();return value as Record<string,unknown>;
}
function keys(value:Record<string,unknown>,allowed:string[]):void {if(Object.keys(value).some(key=>!allowed.includes(key)))throw invalid();}
function identifier(value:unknown):value is string {return typeof value==='string'&&/^[a-zA-Z0-9_-]{1,128}$/.test(value);}
export function parseRunStart(value:unknown):RunStart & {connection:Record<string,unknown>} {
  const p=record(value);
  keys(p,['workerId','contextId','configurationId','purposeFamily','contextDirectory','ledgerDirectory','resume','goal','input','skill','tools','connection']);
  if(![p.workerId,p.contextId,p.configurationId].every(identifier)||!['coordinate','execute','verify'].includes(p.purposeFamily as string)||typeof p.resume!=='boolean')throw invalid();
  for(const field of ['contextDirectory','ledgerDirectory'])if(typeof p[field]!=='string'||!isAbsolute(p[field] as string)||(p[field] as string).includes('\0'))throw invalid();
  for(const field of ['goal','input','skill'])if(typeof p[field]!=='string'||!(p[field] as string).trim()||Buffer.byteLength(p[field] as string)>128*1024)throw invalid();
  if(!Array.isArray(p.tools)||p.tools.length>64)throw invalid();
  for(const item of p.tools) {
    const tool=record(item);keys(tool,['name','description','inputSchema']);
    if(typeof tool.name!=='string'||!/^[a-z][a-z0-9_]{0,63}$/.test(tool.name)||typeof tool.description!=='string'||Buffer.byteLength(tool.description)>4096)throw invalid();
    const schema=record(tool.inputSchema);
    if(schema.type!=='object'||schema.additionalProperties!==false)throw invalid();
  }
  record(p.connection);
  return p as unknown as RunStart & {connection:Record<string,unknown>};
}
export function parseStart(value:unknown):Start {
  const p=parseRunStart(value);
  const c=p.connection;keys(c,['protocol','model','baseUrl','apiKey']);
  if(!['anthropic-messages','openai-chat-completions'].includes(c.protocol as string)||typeof c.model!=='string'||!c.model.trim()||c.model.length>256
    ||typeof c.apiKey!=='string'||!c.apiKey.trim()||Buffer.byteLength(c.apiKey)>16384)throw invalid();
  if(c.baseUrl!==undefined) {
    if(typeof c.baseUrl!=='string'||c.baseUrl.length>2048)throw invalid();
    let url:URL;try {url=new URL(c.baseUrl);} catch {throw invalid();}
    if(url.protocol!=='https:'||url.username||url.password||url.search||url.hash)throw invalid();
  }
  return p as unknown as Start;
}

async function directory(path:string):Promise<void> {
  const stat=await fs.lstat(path);
  if(!stat.isDirectory()||stat.isSymbolicLink()||(stat.mode&0o077)!==0)throw new Error('private_context_directory_invalid');
}
async function syncDirectory(path:string):Promise<void> {
  const handle=await fs.open(path,'r');try {await handle.sync();}finally {await handle.close();}
}
/** The core reserves both private directories before spawn. This manifest
 * prevents reusing a native context for another task, worker or configuration. */
export async function prepareContext(start:Start,scope:Scope):Promise<{store:SQLiteStore;events:JsonlEventStore;checkpoint?:AgentCheckpoint}> {
  await directory(start.contextDirectory);
  await directory(start.ledgerDirectory);
  if(await fs.realpath(start.contextDirectory)===await fs.realpath(start.ledgerDirectory))throw new Error('context_and_ledger_must_be_separate');
  const manifest={version:1,taskId:scope.taskId,workerId:start.workerId,contextId:start.contextId,configurationId:start.configurationId,purposeFamily:start.purposeFamily};
  const manifestPath=join(start.contextDirectory,'binding.json');
  const sqlitePath=join(start.contextDirectory,'state.sqlite3');
  const eventPath=join(start.contextDirectory,'events');
  if(start.resume) {
    for(const file of [manifestPath,sqlitePath]) {
      const stat=await fs.lstat(file);
      if(!stat.isFile()||stat.isSymbolicLink())throw new Error('native_context_missing');
    }
    if((await fs.stat(manifestPath)).size>4096)throw new Error('native_context_binding_invalid');
    const stored=record(JSON.parse(await fs.readFile(manifestPath,'utf8')));
    if(Object.keys(stored).length!==Object.keys(manifest).length||Object.entries(manifest).some(([key,value])=>stored[key]!==value))throw new Error('native_context_binding_mismatch');
    await directory(eventPath);
  } else {
    if((await fs.readdir(start.contextDirectory)).length!==0)throw new Error('native_context_already_exists');
    const file=await fs.open(manifestPath,'wx',0o600);
    try {await file.writeFile(JSON.stringify(manifest));await file.sync();}finally {await file.close();}
    await fs.mkdir(eventPath,{mode:0o700});await syncDirectory(start.contextDirectory);
  }
  const store=new SQLiteStore({path:sqlitePath});
  try {
    await store.init();
    await fs.chmod(sqlitePath,0o600);
    const events=new JsonlEventStore(eventPath);
    let checkpoint:AgentCheckpoint|undefined;
    if(start.resume) {
      const previous=await store.get(`context:${start.contextId}:checkpoint-run:latest`);
      if(!identifier(previous))throw new Error('native_checkpoint_missing');
      const eventFile=join(eventPath,previous+'.jsonl');
      const stat=await fs.lstat(eventFile);
      if(!stat.isFile()||stat.isSymbolicLink()||stat.size>64*1024*1024)throw new Error('native_checkpoint_file_invalid');
      const event=(await events.readByRunId(previous)).filter(item=>item.type==='agent.checkpoint').at(-1);
      checkpoint=(event?.payload as {checkpoint?:AgentCheckpoint}|undefined)?.checkpoint;
      if(!checkpoint||checkpoint.meta.contextId!==start.contextId||checkpoint.meta.agentId!==start.workerId)throw new Error('native_checkpoint_binding_mismatch');
    }
    return {store,events,checkpoint};
  } catch(error) {store.close();throw error;}
}

export async function runApiProcess(channel:AdapterChannel):Promise<void> {
  const start=parseStart(await channel.start());
  const context=await prepareContext(start,channel.scope);
  // Do not pick up the host's ambient provider credential or emit provider
  // exception bodies into ordinary process logs. Secrets only arrive on pipe.
  process.env.LOG_LEVEL='silent';
  try {
    const ledger=await ToolLedger.openDelivery(start.ledgerDirectory,channel.scope.deliveryId);
    const parsed=resolveAndParseConnection({contractVersion:2,legacyEnv:{},fields:{transport:'api',...start.connection}});
    const {gateway,adapterFamily}=assembleApiGateway(parsed);
    const result=await executeApiTurn({
      workerId:start.workerId,taskId:channel.scope.taskId,runId:channel.scope.runId,contextId:start.contextId,
      goal:start.goal,input:start.input,skill:start.skill,model:{provider:start.connection.protocol,adapter:adapterFamily,model:start.connection.model},
      tools:start.tools,gateway,stateStore:context.store,eventStore:context.events,checkpoint:context.checkpoint,
      ledger,forward:channel.forward,signal:channel.signal,
    });
    const stopReason:Terminal['stopReason']=result.stopReason==='model_stop'?'completed':
      ['cancelled','deadline','budget_exhausted'].includes(result.stopReason)?result.stopReason as Terminal['stopReason']:'failed';
    await channel.finish({stopReason,stopCode:result.stopCode,nativeStopReason:result.result.stopReason,recoveredOperations:result.recoveredOperations});
  } finally {context.store.close();}
}
