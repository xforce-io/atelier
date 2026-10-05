/** Trusted local launcher; never runs inside the member container. The first
 * private line contains isolation policy, followed by the member protocol. */
import { launchContainers, parseIsolation, type Containers, type Isolation } from './cli-container.js';

process.umask(0o077);

async function bootstrap(): Promise<Isolation> {
  return new Promise((resolve,reject)=>{
    let buffer=Buffer.alloc(0);
    const finish=(error?:Error,value?:Isolation)=>{
      clearTimeout(timer);process.stdin.off('data',data);process.stdin.off('end',end);
      error?reject(error):resolve(value!);
    };
    const end=()=>finish(new Error('cli_bootstrap_missing'));
    const data=(chunk:Buffer)=>{
      buffer=Buffer.concat([buffer,chunk]);
      const newline=buffer.indexOf(10);
      if(newline<0){if(buffer.length>8192)finish(new Error('cli_bootstrap_too_large'));return;}
      process.stdin.pause();
      if(newline>8192){finish(new Error('cli_bootstrap_too_large'));return;}
      try{
        const value=parseIsolation(JSON.parse(new TextDecoder('utf-8',{fatal:true}).decode(buffer.subarray(0,newline))));
        const remaining=buffer.subarray(newline+1);
        if(remaining.length)process.stdin.unshift(remaining);
        finish(undefined,value);
      }catch{finish(new Error('cli_bootstrap_invalid'));}
    };
    const timer=setTimeout(()=>finish(new Error('cli_bootstrap_timeout')),10000);
    process.stdin.on('data',data);process.stdin.once('end',end);
  });
}
const controller=new AbortController();
let containers:Containers|undefined;
const stop=()=>{
  controller.abort();
  containers?.execution.stdin.destroy();containers?.proxy.stdin.destroy();
  containers?.execution.kill();containers?.proxy.kill();
};
process.once('SIGTERM',stop);process.once('SIGINT',stop);
process.stdin.on('error',stop);process.stdout.on('error',stop);
try{
  if(process.argv.length!==2 && !(process.argv.length===4 && process.argv[2]==='--atelier-run' && /^[0-9a-f-]{36}$/.test(process.argv[3]!)))throw new Error('cli_launcher_arguments_invalid');
  const options=await bootstrap();
  if(process.argv[3]!==undefined && process.argv[3]!==options.runId)throw new Error('cli_launcher_run_mismatch');
  containers=await launchContainers(options,controller.signal);
  const {execution,proxy}=containers;
  const exited=new Promise<number>(resolve=>{execution.once('exit',code=>resolve(code??1));execution.once('error',()=>resolve(1));});
  proxy.once('exit',()=>{if(execution.exitCode===null)stop();});
  proxy.once('error',stop);
  process.stdin.pipe(execution.stdin);execution.stdout.pipe(process.stdout,{end:false});
  process.stdin.resume();
  process.exitCode=await exited;
}catch(error){
  const code=error instanceof Error && /^cli_[a-z_]+$/.test(error.message)?error.message:'cli_launcher_failed';
  process.stderr.write(`Atelier CLI isolation launcher failed (${code}); inspect owned resources.\n`);process.exitCode=1;
}finally{
  stop();process.stdin.destroy();
}
