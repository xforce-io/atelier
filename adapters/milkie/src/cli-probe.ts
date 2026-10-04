import type { ExecutionClient } from '@freemanxu/milkie';
import { setTimeout as delay } from 'node:timers/promises';

export interface CliDiagnostic { runtime:'pi'|'grok-cli'; model:string|null; challenge:string; }
export interface CliProbeOutcome { state:'passed'|'failed'|'inconclusive'; code:string; }
export function parseCliDiagnostic(value:unknown):CliDiagnostic {
  if(!value||typeof value!=='object'||Array.isArray(value))throw new Error('cli_probe_invalid');
  const p=value as Record<string,unknown>;
  if(Object.keys(p).sort().join(',')!=='challenge,model,runtime'||!['pi','grok-cli'].includes(p.runtime as string)
    ||typeof p.challenge!=='string'||! /^[0-9a-f-]{36}$/.test(p.challenge)
    ||(p.model!==null&&(typeof p.model!=='string'||!p.model.trim()||p.model.startsWith('-')||p.model.length>256)))throw new Error('cli_probe_invalid');
  return p as unknown as CliDiagnostic;
}

/** A real native/model tool roundtrip with a fresh challenge. The only tool
 * has no external effects. Neither model output nor provider errors escape. */
export async function probeCli(execution:ExecutionClient,contextId:string,challenge:string,signal:AbortSignal):Promise<CliProbeOutcome> {
  const result=(state:CliProbeOutcome['state'],code:string)=>({state,code});
  if(signal.aborted)return result('inconclusive','cancelled');
  const capabilities=execution.capabilities();
  if(!capabilities.supported||!capabilities.hostTools||!capabilities.modelIterations||!capabilities.cancel||!capabilities.resume||!capabilities.forwarding.includes('serial'))return result('failed','cli_native_failed');
  let id:string|undefined;let calls=0;let valid=false;let cancellation:Promise<unknown>|undefined;
  const stop=()=>{if(id&&!cancellation)cancellation=execution.cancel(id).catch(()=>{});};
  signal.addEventListener('abort',stop,{once:true});
  try {
    id=execution.start(contextId,JSON.stringify({connectionProbe:{instruction:'Call connection_probe exactly once with the challenge below, then finish with a short acknowledgment. No other actions.',challenge}}),{
      tools:[{name:'connection_probe',description:'Confirm this isolated connection check; no business side effects.',inputSchema:{type:'object',properties:{challenge:{type:'string'}},required:['challenge'],additionalProperties:false}}],forwarding:'serial',timeoutMs:120000,maxModelIterations:50
    },call=>{
      calls++;
      const input=call.input;
      if(calls!==1||signal.aborted||call.runId!==id||call.contextId!==contextId||call.name!=='connection_probe'
        ||!input||typeof input!=='object'||Array.isArray(input)||Object.keys(input).join(',')!=='challenge'||(input as {challenge:unknown}).challenge!==challenge){
        valid=false;stop();return {ok:false,code:'rejected',message:'Connection diagnostic rejected.'};
      }
      valid=true;return {ok:true,output:'Connection diagnostic received.'};
    });
    const deadline=Date.now()+125000;
    while(true){
      const record=execution.query(id);
      if(!record)return result('inconclusive','adapter_failed');
      if(record.stopped){
        if(signal.aborted)return result('inconclusive','cancelled');
        if(record.status==='timed_out')return result('inconclusive','deadline');
        if(record.status!=='succeeded')return result('failed','cli_native_failed');
        return valid&&calls===1?result('passed','cli_tool_roundtrip'):result('failed','cli_tool_unverified');
      }
      if(record.status==='unknown'||signal.aborted||Date.now()>=deadline){stop();await cancellation;return result('inconclusive',signal.aborted?'cancelled':'deadline');}
      await delay(50);
    }
  }catch{return result('failed','cli_native_failed');}
  finally{signal.removeEventListener('abort',stop);stop();await cancellation;}
}
