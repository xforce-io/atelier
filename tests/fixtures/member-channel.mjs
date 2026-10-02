// Explicit transport fixture, never a production model or acceptance agent.
import { AdapterChannel } from '../../adapters/milkie/dist/src/channel.js';
const channel=await AdapterChannel.connect(process.stdin,process.stdout);
const start=await channel.start();
if(start.mode==='wrong_scope') {
  process.stdout.write(JSON.stringify({protocolVersion:1,...channel.scope,taskId:'other-task',requestId:'forged',seq:2,kind:'tool.request',payload:{}})+'\n');
  process.stdin.destroy();
} else if(start.mode==='gap') {
  process.stdout.write(JSON.stringify({protocolVersion:1,...channel.scope,requestId:'terminal',seq:8,kind:'terminal',payload:{stopReason:'completed',nativeStopReason:'model_stop',recoveredOperations:0}})+'\n');
  process.stdin.destroy();
} else if(start.mode==='conflict') {
  process.stdout.write(JSON.stringify({protocolVersion:1,...channel.scope,requestId:'hello',seq:1,kind:'ready',payload:{changed:true}})+'\n');
  process.stdin.destroy();
} else if(start.mode==='truncated') {
  process.stdout.write('{"protocolVersion":');
  process.stdin.destroy();
} else {
  for(const operation of start.operations) {
    const result=await channel.forward(operation);
    if(!result.ok) throw new Error('fixture_core_rejected');
  }
  if(start.mode==='respond') {
    const result=await channel.forward({operationId:'response',originatingRunId:channel.scope.runId,toolCallId:'response-call',name:'message_respond',input:{kind:'wait',reason:'测试成员等待补充规则',handler:start.operations[0].input.recipient}});
    if(!result.ok)throw new Error('fixture_disposition_rejected');
  }
  if(start.mode==='blocker') {
    const result=await channel.forward({operationId:'blocker',originatingRunId:channel.scope.runId,toolCallId:'blocker-call',name:'task_report_blocker',input:{handler:start.operations[0].input.recipient,reason:'真实子进程报告测试阻塞并等待服务停止'}});
    if(!result.ok)throw new Error('fixture_blocker_rejected');
    if(!channel.signal.aborted)await new Promise(resolve=>channel.signal.addEventListener('abort',resolve,{once:true}));
    await channel.finish({stopReason:'cancelled',nativeStopReason:'abort_signal',recoveredOperations:0});
  } else {
    await channel.finish({stopReason:'completed',nativeStopReason:'model_stop',recoveredOperations:0});
  }
}
