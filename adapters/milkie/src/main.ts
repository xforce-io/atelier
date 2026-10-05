import { AdapterChannel } from './channel.js';
import { runApiProcess } from './api-process.js';
import { runCliProcess } from './cli-process.js';

// Dedicated child only: optional core-owned process marker, no ambient
// connection discovery or shell. The marker never supplies channel identity.
// Never stringify arbitrary exceptions: they can contain provider response data.
process.umask(0o077);
process.env.LOG_LEVEL='silent';
let channel:AdapterChannel|undefined;
try {
  if(process.argv.length!==2 && !(process.argv.length===4 && process.argv[2]==='--atelier-run' && /^[0-9a-f-]{36}$/.test(process.argv[3]!)))throw new Error('unexpected_arguments');
  channel=await AdapterChannel.connect(process.stdin,process.stdout);
  if(channel.transport==='agent-cli') await runCliProcess(channel);
  else await runApiProcess(channel);
} catch {
  if(channel)await channel.finish({stopReason:'failed',stopCode:'ADAPTER_FAILED',nativeStopReason:'runtime_error',recoveredOperations:0}).catch(()=>{});
  process.stderr.write('Atelier adapter failed; inspect bound run and private state.\n');
  process.exitCode=1;
  process.stdin.destroy();
}
