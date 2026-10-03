import { AsyncLocalStorage } from 'node:async_hooks';
import {
  AgentRuntime, DefaultIOPort, NoopRecorder,
  type AgentCheckpoint, type AgentResult, type ICrashSafeEventStore, type IModelGateway,
  type IStateStore, type StopReason, type ModelConfig, type ToolDefinition, type ToolInvocationOptions, type ToolSchema,
} from '@freemanxu/milkie';
import { ToolLedger, type ForwardTool } from './tool-ledger.js';

export interface ApiTurn {
  workerId: string;
  taskId: string;
  runId: string;
  contextId: string;
  goal: string;
  input: string;
  skill: string;
  model: ModelConfig;
  tools: ToolSchema[];
  gateway: IModelGateway;
  stateStore: IStateStore;
  eventStore: ICrashSafeEventStore;
  ledger: ToolLedger;
  forward: ForwardTool;
  reconcile?: ForwardTool;
  checkpoint?: AgentCheckpoint;
  signal?: AbortSignal;
}

/** A single API turn in milkie. Core owns identity, scheduling and all business
 * state. This function never invokes the management CLI or selects a next role. */
export async function executeApiTurn(turn: ApiTurn): Promise<{ result: AgentResult; stopReason: StopReason; stopCode?: string; recoveredOperations: number }> {
  if ([turn.workerId,turn.taskId,turn.runId,turn.contextId].some(id=>!id || !/^[a-zA-Z0-9_-]{1,128}$/.test(id))) throw new Error('run_binding_invalid');
  if (!turn.model.model || !turn.skill || turn.tools.length > 64) throw new Error('run_configuration_invalid');
  const names = turn.tools.map(tool => tool.name);
  if (new Set(names).size !== names.length || names.some(name => !/^[a-z][a-z0-9_]{0,63}$/.test(name))) throw new Error('run_tools_invalid');
  if (turn.checkpoint && (turn.checkpoint.meta.contextId !== turn.contextId || turn.checkpoint.meta.agentId !== turn.workerId)) throw new Error('checkpoint_binding_mismatch');
  const controller = new AbortController();
  const deadlineAt = Date.now()+15*60*1000;
  let deadlineExceeded = false;
  const deadlineTimer = setTimeout(()=>{deadlineExceeded=true;controller.abort();},15*60*1000);
  const relay = () => controller.abort();
  if (turn.signal?.aborted) controller.abort();
  turn.signal?.addEventListener('abort',relay,{once:true});
  const callContext = new AsyncLocalStorage<string>();
  let transportFailed = false;
  let toolCalls = 0;
  let toolBudgetExceeded = false;
  const forward: ForwardTool = async operation => {
    if (controller.signal.aborted) throw new Error('run_cancelled');
    let onAbort: (()=>void) | undefined;
    try {
      return await new Promise<unknown>((resolve,reject)=>{
        onAbort=()=>reject(new Error('tool_request_aborted'));
        controller.signal.addEventListener('abort',onAbort,{once:true});
        Promise.resolve().then(()=>{
          if(controller.signal.aborted) throw new Error('tool_request_aborted');
          return turn.forward(operation,controller.signal);
        }).then(resolve,reject);
      });
    }
    catch {
      // After a lost tool result the model must not generate a replacement call.
      transportFailed = true;
      controller.abort();
      throw new Error('tool_result_uncertain');
    } finally { if(onAbort) controller.signal.removeEventListener('abort',onAbort); }
  };
  class BoundPort extends DefaultIOPort {
    override async invokeTool(name: string, input: unknown, execute: (signal: AbortSignal) => Promise<unknown>, options?: ToolInvocationOptions): Promise<unknown> {
      if (!options?.toolCallId) throw new Error('stable_tool_call_id_missing');
      toolCalls += 1;
      if (toolCalls > 100) {
        toolBudgetExceeded = true;
        controller.abort();
        throw new Error('tool_call_budget_exceeded');
      }
      // Execute milkie's own dispatch gate; do not forward rejected or budget-
      // exhausted thunks directly to the core.
      return callContext.run(options.toolCallId, () => super.invokeTool(name,input,execute,options));
    }
  }
  const tools: ToolDefinition[] = turn.tools.map(tool => ({
    ...tool,
    parallelSafe: false,
    handler: async (input, context) => {
      if (context.signal.aborted || controller.signal.aborted) throw new Error('run_cancelled');
      const callId = callContext.getStore();
      if (!callId) throw new Error('stable_tool_call_id_missing');
      try { return await turn.ledger.call(turn.runId,callId,tool.name,input,forward); }
      catch {
        transportFailed = true;
        controller.abort();
        throw new Error('tool_result_uncertain');
      }
    },
  }));
  try {
    // Transport recovery precedes any new model invocation. An earlier Run's
    // recorded operation retains its ID; new model calls receive new IDs.
    // A crash after durable reconciliation but before checkpointing the next
    // input must not lose that result. Recheck even completed records under
    // this delivery's current authorization before exposing them to the model.
    const recovered = [...turn.ledger.foreignRecovery, ...await turn.ledger.recover(turn.reconcile??forward,true)];
    const recoveredOperations = recovered.length;
    const input = recovered.length === 0 ? turn.input : JSON.stringify({workMessage:turn.input,reconciledOperations:recovered});
    const runtime = new AgentRuntime({
      config: {
        agentId:turn.workerId, version:'atelier-api-v1', systemPrompt:turn.skill,
        model:turn.model, builtinTools:{allow:[]},
        fsm:{states:[{name:'work',type:'llm',tools:names,max_iterations:50}],max_tool_calls:100},
      },
      goal:turn.goal,input,contextId:turn.contextId,agentRunId:turn.runId,
      variables:{atelierTaskId:turn.taskId},
      stateStore:turn.stateStore,eventStore:turn.eventStore,recorder:new NoopRecorder(),
      ioPort:new BoundPort(turn.gateway),extraTools:tools,
      control:{signal:controller.signal,deadlineAt},
    });
    if (runtime.getEffectiveBuiltinTools().length !== 0) throw new Error('builtin_tools_not_disabled');
    if (turn.checkpoint) await runtime.loadCheckpoint(turn.checkpoint);
    const result = await runtime.run(input);
    await turn.eventStore.confirmRunDurable(turn.runId);
    if (transportFailed) throw new Error('tool_result_uncertain');
    if(result.stopReason==='model_stop')await turn.ledger.confirmForeignRecovery();
    return {result,recoveredOperations,stopReason:toolBudgetExceeded?'budget_exhausted':deadlineExceeded?'deadline':result.stopReason,stopCode:toolBudgetExceeded?'TOOL_CALL_BUDGET_EXCEEDED':deadlineExceeded?'RUN_DEADLINE_EXCEEDED':result.stopCode};
  } finally {
    clearTimeout(deadlineTimer);
    turn.signal?.removeEventListener('abort',relay);
  }
}
