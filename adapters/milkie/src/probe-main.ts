import { parseProbe, probeConnection, type ProbeResult } from './connection-probe.js';

process.umask(0o077);
process.env.LOG_LEVEL='silent';
const controller=new AbortController();
let completed=false;
function finish(result:ProbeResult):void {
  if(completed)return;
  completed=true;controller.abort();process.stdin.destroy();
  process.stdout.write(JSON.stringify(result)+'\n',()=>process.exit(0));
}
// Parent also enforces a 30-second limit and reaps its actual Child. The local
// bound makes an orphaned probe finite even after parent SIGKILL.
setTimeout(()=>finish({state:'inconclusive',code:'deadline'}),25_000);
process.stdin.on('end',()=>finish({state:'inconclusive',code:'cancelled'}));
process.stdin.on('error',()=>finish({state:'inconclusive',code:'cancelled'}));
let bytes=Buffer.alloc(0), started=false;
process.stdin.on('data',(chunk:Buffer)=>{
  if(started || bytes.length+chunk.length>32*1024) {finish({state:'inconclusive',code:'invalid_response'});return;}
  bytes=Buffer.concat([bytes,chunk]);
  const newline=bytes.indexOf(10);
  if(newline===-1)return;
  if(newline!==bytes.length-1) {finish({state:'inconclusive',code:'invalid_response'});return;}
  started=true;
  void (async()=>{
    try {
      if(process.argv.length!==2)throw new Error('invalid_arguments');
      const connection=parseProbe(JSON.parse(bytes.subarray(0,newline).toString('utf8')));
      bytes=Buffer.alloc(0);
      finish(await probeConnection(connection,controller.signal));
    } catch {finish({state:'inconclusive',code:'invalid_response'});}
  })();
});
