/** Trusted host process. No business channel; durable resource ownership and
 * its creation permit are established by Rust before this bootstrap arrives. */
import { launchContainers, parseIsolation, type Containers, type Isolation } from './cli-container.js';
import { parseCliDiagnostic, type CliDiagnostic } from './cli-probe.js';
process.umask(0o077);
const controller=new AbortController();let containers:Containers|undefined;
const stop=()=>{controller.abort();containers?.execution.stdin.destroy();containers?.proxy.stdin.destroy();containers?.execution.kill();containers?.proxy.kill();};
process.once('SIGTERM',stop);process.once('SIGINT',stop);process.stdin.once('end',stop);process.stdin.on('error',stop);process.stdout.on('error',stop);
const deadline=setTimeout(stop,145000);
try {
  if(process.argv.length!==4||process.argv[2]!=='--atelier-probe'||!/^[0-9a-f-]{36}$/.test(process.argv[3]!))throw new Error('invalid');
  const bootstrap=await new Promise<{isolation:Isolation;diagnostic:CliDiagnostic}>((resolve,reject)=>{
    let buffer=Buffer.alloc(0);
    const done=(error?:Error,value?:{isolation:Isolation;diagnostic:CliDiagnostic})=>{clearTimeout(timer);process.stdin.off('data',data);controller.signal.removeEventListener('abort',abort);error?reject(error):resolve(value!);};
    const abort=()=>done(new Error('cancelled'));
    const data=(chunk:Buffer)=>{
      buffer=Buffer.concat([buffer,chunk]);if(buffer.length>16384){done(new Error('invalid'));return;}
      const newline=buffer.indexOf(10);if(newline<0)return;
      try{
        if(newline!==buffer.length-1)throw new Error('invalid');
        const value=JSON.parse(new TextDecoder('utf-8',{fatal:true}).decode(buffer.subarray(0,newline)));
        if(Object.keys(value).sort().join(',')!=='diagnostic,isolation')throw new Error('invalid');
        const isolation=parseIsolation(value.isolation),diagnostic=parseCliDiagnostic(value.diagnostic);
        if(isolation.runId!==process.argv[3])throw new Error('invalid');done(undefined,{isolation,diagnostic});
      }catch{done(new Error('invalid'));}
    };
    const timer=setTimeout(abort,10000);process.stdin.on('data',data);controller.signal.addEventListener('abort',abort,{once:true});if(controller.signal.aborted)abort();
  });
  containers=await launchContainers(bootstrap.isolation,controller.signal,'probe');
  const {execution,proxy}=containers;
  const exited=new Promise<number>(resolve=>{execution.once('exit',code=>resolve(code??1));execution.once('error',()=>resolve(1));});
  proxy.once('exit',stop);proxy.once('error',stop);
  let bytes=0;
  execution.stdout.on('data',(chunk:Buffer)=>{bytes+=chunk.length;if(bytes>4096)stop();else process.stdout.write(chunk);});
  execution.stdin.write(JSON.stringify(bootstrap.diagnostic)+'\n');process.stdin.resume();
  process.exitCode=await exited;
}catch{process.exitCode=1;}
finally{clearTimeout(deadline);stop();process.stdin.destroy();}
