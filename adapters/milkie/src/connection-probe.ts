import { assembleApiGateway, resolveAndParseConnection, type IModelGateway } from '@freemanxu/milkie';

export interface ProbeConnection {
  protocol:'anthropic-messages'|'openai-chat-completions'; model:string; baseUrl?:string; apiKey:string;
}
export interface ProbeResult {
  state:'passed'|'failed'|'inconclusive';
  code:'response_received'|'invalid_response'|'provider_rejected'|'connection_failed'|'deadline'|'cancelled';
  httpStatus?:number;
}
export function parseProbe(value:unknown):ProbeConnection {
  if(!value || typeof value!=='object' || Array.isArray(value))throw new Error('invalid_probe');
  const p=value as Record<string,unknown>;
  if(Object.keys(p).some(k=>!['protocol','model','baseUrl','apiKey'].includes(k)) ||
    !['anthropic-messages','openai-chat-completions'].includes(p.protocol as string) ||
    typeof p.model!=='string' || !p.model.trim() || p.model.length>256 ||
    typeof p.apiKey!=='string' || !p.apiKey.trim() || Buffer.byteLength(p.apiKey)>16384 || /[\x00-\x1f\x7f]/.test(p.apiKey))throw new Error('invalid_probe');
  if(p.baseUrl!==undefined) {
    if(typeof p.baseUrl!=='string'||p.baseUrl.length>2048)throw new Error('invalid_probe');
    const u=new URL(p.baseUrl);
    if(u.protocol!=='https:'||u.username||u.password||u.search||u.hash)throw new Error('invalid_probe');
  }
  return p as unknown as ProbeConnection;
}

/** No AgentRuntime, Task, tools, checkpoint or provider response text is needed
 * to establish that this exact configured model can answer a minimal request. */
export async function probeGateway(gateway:IModelGateway, model:string, signal:AbortSignal):Promise<ProbeResult> {
  if(signal.aborted)return {state:'inconclusive',code:'cancelled'};
  try {
    const response=await gateway.complete({model,messages:[{role:'user',content:[{type:'text',text:'Reply with OK.'}]}],maxTokens:64},{signal});
    if(signal.aborted)return {state:'inconclusive',code:'cancelled'};
    if(!Array.isArray(response.content)||!response.content.some(c=>c.type==='text'&&c.text.trim())||!Array.isArray(response.toolCalls)||response.toolCalls.length!==0)
      return {state:'inconclusive',code:'invalid_response'};
    return {state:'passed',code:'response_received'};
  } catch(error) {
    if(signal.aborted)return {state:'inconclusive',code:'cancelled'};
    // Never serialize message/raw/cause: SDK errors can contain credentials,
    // response bodies or user-controlled endpoint text. Copy only numeric status.
    const status=error && typeof error==='object' ? (error as {status?:unknown;envelope?:{status?:unknown}}).status ?? (error as {envelope?:{status?:unknown}}).envelope?.status : undefined;
    if(typeof status==='number'&&Number.isInteger(status)&&status>=400&&status<=599)
      return {state:'failed',code:'provider_rejected',httpStatus:status};
    return {state:'failed',code:'connection_failed'};
  }
}

export async function probeConnection(connection:ProbeConnection, signal:AbortSignal):Promise<ProbeResult> {
  const parsed=resolveAndParseConnection({contractVersion:2,legacyEnv:{},fields:{transport:'api',...connection}});
  const {gateway}=assembleApiGateway(parsed);
  return probeGateway(gateway,connection.model,signal);
}
