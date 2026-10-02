/** One private configuration line, then keep stdin open for resource ownership. */
import {networkInterfaces} from 'node:os';
import {isIP} from 'node:net';
import {createEgressProxy,policyHosts} from './egress-proxy.js';

let configured=false;
let bytes=Buffer.alloc(0);
let proxy:ReturnType<typeof createEgressProxy>|undefined;
let closing=false;
const stop=async()=>{
  if(closing)return;closing=true;
  if(proxy?.server.listening)await proxy.close().catch(()=>{});
  process.exit(process.exitCode??0);
};
process.once('SIGTERM',()=>void stop());
process.once('SIGINT',()=>void stop());
process.stdin.once('end',()=>{if(!configured)process.exitCode=1;void stop();});
const timeout=setTimeout(()=>{process.exitCode=1;process.stdin.destroy();void stop();},5000);
process.stdin.on('data',(chunk:Buffer)=>{
  if(configured){process.exitCode=1;void stop();return;}
  bytes=Buffer.concat([bytes,chunk]);
  if(bytes.length>8192){process.exitCode=1;void stop();return;}
  const end=bytes.indexOf(10);if(end<0)return;
  configured=true;clearTimeout(timeout);
  try{
    if(end!==bytes.length-1)throw new Error('invalid_config');
    const value=JSON.parse(bytes.subarray(0,end).toString('utf8')) as Record<string,unknown>;
    if(Object.keys(value).sort().join(',')!=='hosts,listenHost'||typeof value.listenHost!=='string'||isIP(value.listenHost)!==4||value.listenHost==='0.0.0.0'||
      !Object.values(networkInterfaces()).flat().some(i=>i?.address===value.listenHost))throw new Error('invalid_config');
    proxy=createEgressProxy(policyHosts(value.hosts));
    proxy.server.once('error',()=>{process.exitCode=1;void stop();});
    proxy.server.listen(3128,value.listenHost,()=>process.stdout.write(JSON.stringify({state:'listening',port:3128})+'\n'));
  }catch{process.exitCode=1;void stop();}
});
