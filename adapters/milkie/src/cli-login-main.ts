/** Private control process for an interactive native login. No credentials or
 * terminal input pass through this control protocol. Rust attaches the TTY. */
import { prepareLoginContainer, parseLoginIsolation, type LoginIsolation } from './cli-container.js';
import type { ChildProcessWithoutNullStreams } from 'node:child_process';

process.umask(0o077);
const controller=new AbortController();
let proxy:ChildProcessWithoutNullStreams|undefined;
let finishLifetime:(()=>void)|undefined;
const lifetime=new Promise<void>(resolve=>{finishLifetime=resolve;});
const stop=()=>{controller.abort();proxy?.stdin.destroy();proxy?.kill();finishLifetime?.();};
process.once('SIGTERM',stop);process.once('SIGINT',stop);
process.stdin.once('end',stop);process.stdin.on('error',stop);process.stdout.on('error',stop);
const deadline=setTimeout(stop,30*60*1000);
try {
  if(process.argv.length!==4||process.argv[2]!=='--atelier-login'||!/^[0-9a-f-]{36}$/.test(process.argv[3]!))throw new Error('cli_login_arguments_invalid');
  const options=await new Promise<LoginIsolation>((resolve,reject)=>{
    let buffer=Buffer.alloc(0);
    const done=(error?:Error,value?:LoginIsolation)=>{clearTimeout(timer);process.stdin.off('data',data);controller.signal.removeEventListener('abort',abort);error?reject(error):resolve(value!);};
    const abort=()=>done(new Error('cli_login_cancelled'));
    const data=(chunk:Buffer)=>{
      buffer=Buffer.concat([buffer,chunk]);
      if(buffer.length>8192){done(new Error('cli_login_bootstrap_invalid'));return;}
      const newline=buffer.indexOf(10);if(newline<0)return;
      try {
        if(newline!==buffer.length-1)throw new Error('extra_input');
        const value=parseLoginIsolation(JSON.parse(new TextDecoder('utf-8',{fatal:true}).decode(buffer.subarray(0,newline))));
        if(value.loginId!==process.argv[3])throw new Error('wrong_login');
        done(undefined,value);
      }catch{done(new Error('cli_login_bootstrap_invalid'));}
    };
    const timer=setTimeout(()=>done(new Error('cli_login_bootstrap_timeout')),10000);
    process.stdin.on('data',data);controller.signal.addEventListener('abort',abort,{once:true});
    if(controller.signal.aborted)abort();
  });
  const prepared=await prepareLoginContainer(options,controller.signal);proxy=prepared.proxy;
  proxy.once('exit',stop);proxy.once('error',stop);
  if(controller.signal.aborted||proxy.exitCode!==null||proxy.signalCode!==null)throw new Error('cli_login_cancelled');
  process.stdout.write(JSON.stringify({state:'created',containerName:prepared.containerName})+'\n');
  await lifetime;
}catch{
  process.stderr.write('Atelier CLI login environment failed; inspect its saved operation.\n');process.exitCode=1;
}finally {clearTimeout(deadline);stop();process.stdin.destroy();}
