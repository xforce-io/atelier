/** Container-only diagnostics. Core mounts a fresh disposable native directory
 * and the selected Worker's login. Never discover the host's ambient login. */
import { promises as fs } from 'node:fs';
import { ExecutionClient } from '@freemanxu/milkie';
import { parseCliDiagnostic, probeCli } from './cli-probe.js';
process.umask(0o077);
const controller=new AbortController();
const stop=()=>controller.abort();
process.once('SIGTERM',stop);process.once('SIGINT',stop);process.stdin.once('end',stop);process.stdin.on('error',stop);process.stdout.on('error',stop);
const deadline=setTimeout(stop,125000);
try {
  const diagnostic=await new Promise<ReturnType<typeof parseCliDiagnostic>>((resolve,reject)=>{
    let buffer=Buffer.alloc(0);
    const done=(error?:Error,value?:ReturnType<typeof parseCliDiagnostic>)=>{process.stdin.off('data',data);controller.signal.removeEventListener('abort',abort);error?reject(error):resolve(value!);};
    const abort=()=>done(new Error('cancelled'));
    const data=(chunk:Buffer)=>{
      buffer=Buffer.concat([buffer,chunk]);
      if(buffer.length>4096){done(new Error('invalid'));return;}
      const newline=buffer.indexOf(10);if(newline<0)return;
      try{if(newline!==buffer.length-1)throw new Error('invalid');done(undefined,parseCliDiagnostic(JSON.parse(new TextDecoder('utf-8',{fatal:true}).decode(buffer.subarray(0,newline)))));}catch{done(new Error('invalid'));}
    };
    process.stdin.on('data',data);controller.signal.addEventListener('abort',abort,{once:true});if(controller.signal.aborted)abort();
  });
  for(const path of ['/state/native','/config']){const s=await fs.lstat(path);if(!s.isDirectory()||s.isSymbolicLink()||(s.mode&0o077)!==0)throw new Error('invalid');}
  if((await fs.readdir('/state/native')).length)throw new Error('invalid');
  for(const name of ['cwd','sessions'])await fs.mkdir('/state/native/'+name,{mode:0o700});
  const execution=new ExecutionClient({dataDir:'/state/native/sdk',connection:{contractVersion:2,legacyEnv:{},fields:{transport:'agent-cli',runtime:diagnostic.runtime,...(diagnostic.model?{model:diagnostic.model}:{})}}});
  const context=execution.createContext('/state/native/cwd',{configDir:'/config',sessionDir:'/state/native/sessions'});
  const result=await probeCli(execution,context.contextId,diagnostic.challenge,controller.signal);
  process.stdout.write(JSON.stringify(result)+'\n');
}catch{process.stdout.write(JSON.stringify({state:'inconclusive',code:'adapter_failed'})+'\n');}
finally{clearTimeout(deadline);stop();process.stdin.destroy();}
