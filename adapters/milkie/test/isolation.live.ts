/** Explicit Docker integration: public example.com is the only external target. */
import assert from 'node:assert/strict';
import {test} from 'node:test';
import {execFile,spawn,type ChildProcessWithoutNullStreams} from 'node:child_process';
import {promisify} from 'node:util';
import {randomUUID,createHash} from 'node:crypto';
import {fileURLToPath} from 'node:url';
import {writeFile,mkdir,readFile} from 'node:fs/promises';
const exec=promisify(execFile);
const image='sha256:7a87e3fe2909d0135d9041760b2808e70b9d2afb368e450e01d96b614665d133';
async function docker(...args:string[]){return (await exec('docker',args,{timeout:25000,maxBuffer:1024*1024})).stdout.trim();}
async function firstLine(child:ChildProcessWithoutNullStreams):Promise<any>{
  return new Promise((resolve,reject)=>{
    const timer=setTimeout(()=>reject(new Error('container_ready_timeout')),10000);let text='';
    const failed=()=>{clearTimeout(timer);reject(new Error('container_exited_before_ready'));};
    child.once('exit',failed);child.stdout.on('data',chunk=>{
      text+=chunk.toString();if(text.length>8192){clearTimeout(timer);reject(new Error('oversized_ready'));return;}
      const end=text.indexOf('\n');if(end>=0){clearTimeout(timer);child.removeListener('exit',failed);try{resolve(JSON.parse(text.slice(0,end)));}catch(e){reject(e);}}
    });
  });
}
const limits=['--pull=never','--user','65534:65534','--read-only','--cap-drop','ALL','--security-opt','no-new-privileges','--pids-limit','32','--cpus','1','--memory','128m'];
test('real isolated bridge blocks gateway/direct/DNS bypass and only exposes approved CONNECT', {timeout:90000},async()=>{
  const id=randomUUID();const inner=`atelier-inner-${id}`,outer=`atelier-outer-${id}`,proxyName=`atelier-proxy-${id}`,hostName=`atelier-host-fixture-${id}`;
  const containers:string[]=[];const networks:string[]=[];const children:ChildProcessWithoutNullStreams[]=[];
  const label=['--label',`atelier.test=${id}`];
  try{
    await docker('network','create','--internal','--driver','bridge','--opt','com.docker.network.bridge.inhibit_ipv4=true',...label,inner);networks.push(inner);
    await docker('network','create',...label,outer);networks.push(outer);
    const innerInfo=JSON.parse(await docker('network','inspect',inner))[0];
    const outerInfo=JSON.parse(await docker('network','inspect',outer))[0];
    assert.equal(innerInfo.Internal,true);assert.equal(innerInfo.EnableIPv6,false);
    assert.equal(innerInfo.Options['com.docker.network.bridge.inhibit_ipv4'],'true');
    const innerGateway=innerInfo.IPAM.Config[0].Gateway as string,outerGateway=outerInfo.IPAM.Config[0].Gateway as string;
    await docker('create','--name',hostName,'--network','host',...label,...limits,'--entrypoint','node',image,'-e',`const net=require('net'),os=require('os');const s=net.createServer(c=>c.end('HOST_FIXTURE'));s.listen(0,'0.0.0.0',()=>console.log(JSON.stringify({port:s.address().port,hasInnerGateway:Object.values(os.networkInterfaces()).flat().some(i=>i.address===${JSON.stringify(innerGateway)})})));`);containers.push(hostName);
    const host=spawn('docker',['start','-a',hostName],{stdio:['pipe','pipe','pipe']});children.push(host);host.stderr.resume();
    const hostReady=await firstLine(host);assert.equal(hostReady.hasInnerGateway,false,'internal bridge must have no host IP');
    const connectScript=(address:string,port:number)=>`const net=require('net');const s=net.connect({host:${JSON.stringify(address)},port:${port}});let connected=false;s.on('connect',()=>connected=true);s.setTimeout(1200);s.once('data',d=>{console.log(JSON.stringify({connected:true,data:d.toString()}));s.destroy()});s.once('error',()=>console.log(JSON.stringify({connected:false})));s.once('timeout',()=>{console.log(JSON.stringify({connected}));s.destroy()});`;
    async function client(code:string,network=inner){return JSON.parse(await docker('run','--rm','--network',network,...limits,'--sysctl','net.ipv6.conf.all.disable_ipv6=1','--entrypoint','node',image,'-e',code));}
    assert.equal((await client(connectScript(outerGateway,hostReady.port),outer)).data,'HOST_FIXTURE','positive control: actual host fixture is reachable from ordinary bridge');
    assert.equal((await client(connectScript(innerGateway,hostReady.port))).connected,false);
    assert.equal((await client(connectScript(outerGateway,hostReady.port))).connected,false);
    assert.equal((await client(connectScript('1.1.1.1',443))).connected,false);
    const dns=await client(`require('dns').lookup('example.com',{family:4},e=>{console.log(JSON.stringify({resolved:!e}));process.exit(0)});setTimeout(()=>{console.log(JSON.stringify({resolved:false}));process.exit(0)},2500)`);assert.equal(dns.resolved,false);
    const main=fileURLToPath(new URL('../src/proxy-main.js',import.meta.url));const implementation=fileURLToPath(new URL('../src/egress-proxy.js',import.meta.url));
    await docker('create','-i','--name',proxyName,'--network',inner,...label,...limits,'--mount',`type=bind,source=${main},target=/adapter/proxy-main.mjs,readonly`,'--mount',`type=bind,source=${implementation},target=/adapter/egress-proxy.js,readonly`,'--entrypoint','node',image,'--experimental-default-type=module','/adapter/proxy-main.mjs');containers.push(proxyName);
    await docker('network','connect',outer,proxyName);
    const inspected=JSON.parse(await docker('inspect',proxyName))[0];
    assert.equal(inspected.HostConfig.Privileged,false);assert.equal(inspected.HostConfig.ReadonlyRootfs,true);
    const proxy=spawn('docker',['start','-ai',proxyName],{stdio:['pipe','pipe','pipe']});children.push(proxy);proxy.stderr.resume();
    const ready=firstLine(proxy);let proxyIp='';const deadline=Date.now()+4000;
    while(!proxyIp){proxyIp=JSON.parse(await docker('inspect',proxyName))[0].NetworkSettings.Networks[inner].IPAddress;if(!proxyIp){assert.ok(Date.now()<deadline,'proxy network attachment');await new Promise(r=>setTimeout(r,25));}}
    proxy.stdin.write(JSON.stringify({listenHost:proxyIp,hosts:['example.com']})+'\n');assert.equal((await ready).state,'listening');
    function tunnel(authority:string,tls:boolean){return `const net=require('net'),tls=require('tls');const s=net.connect({host:${JSON.stringify(proxyIp)},port:3128});let status=0;s.setTimeout(18000,()=>{console.log(JSON.stringify({error:'timeout',status}));process.exit(0)});let b='';s.on('error',()=>{console.log(JSON.stringify({error:'connection'}));process.exit(0)});s.on('connect',()=>s.write('CONNECT '+${JSON.stringify(authority)}+' HTTP/1.1\\r\\nHost: '+${JSON.stringify(authority)}+'\\r\\n\\r\\n'));function header(d){b+=d.toString();if(!b.includes('\\r\\n\\r\\n'))return;s.removeListener('data',header);status=Number(b.split(' ')[1]);if(status!==200||!${tls}){console.log(JSON.stringify({status}));s.destroy();return;}const t=tls.connect({socket:s,servername:'example.com'},()=>t.write('HEAD / HTTP/1.1\\r\\nHost: example.com\\r\\nConnection: close\\r\\n\\r\\n'));t.on('error',()=>{console.log(JSON.stringify({error:'tls'}));process.exit(0)});let result='';t.on('data',d=>{result+=d.toString();if(result.includes('\\r\\n')){console.log(JSON.stringify({status,httpsStatus:Number(result.split(' ')[1])}));t.destroy();}});}s.on('data',header);`;}
    assert.equal((await client(tunnel('unapproved.example:443',false))).status,403);
    assert.equal((await client(tunnel('127.0.0.1:443',false))).status,403);
    const accepted=await client(tunnel('example.com:443',true));assert.equal(accepted.status,200,JSON.stringify(accepted));assert.ok(accepted.httpsStatus>=200&&accepted.httpsStatus<400,JSON.stringify(accepted));
    proxy.stdin.end();await new Promise<void>((resolve,reject)=>{const timer=setTimeout(()=>reject(new Error('proxy_did_not_stop')),5000);proxy.once('exit',code=>{clearTimeout(timer);code===0?resolve():reject(new Error('proxy_exit'));});});
    assert.equal(JSON.parse(await docker('inspect',proxyName))[0].State.Running,false);
    const evidence=fileURLToPath(new URL(`../../../../.agents/verify-runs/1/isolation-${id}.json`,import.meta.url));await mkdir(fileURLToPath(new URL('../../../../.agents/verify-runs/1/',import.meta.url)),{recursive:true});
    const sources=Object.fromEntries(await Promise.all([main,implementation].map(async path=>[path,createHash('sha256').update(await readFile(path)).digest('hex')])));
    await writeFile(evidence,JSON.stringify({kind:'development_integration',sources,image,internal:true,inhibitIpv4:true,hostPositiveControl:true,gatewayBlocked:true,directBlocked:true,dnsBlocked:true,unapprovedDenied:true,allowedPublicHttps:accepted,proxyStopped:true,cliExecution:false},null,2));
    console.log(`evidence: ${evidence}`);
  }finally{
    for(const name of containers.reverse())await docker('rm','-f',name).catch(()=>{});
    for(const child of children){child.stdin.destroy();child.kill();}
    for(const name of networks.reverse())await docker('network','rm',name).catch(()=>{});
  }
});
