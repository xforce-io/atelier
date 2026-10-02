/** Trusted CONNECT relay. The caller owns its isolated network and lifecycle. */
import {createServer} from 'node:http';
import {lookup as dnsLookup} from 'node:dns/promises';
import {connect as tcpConnect, isIP, Socket} from 'node:net';
import type {Duplex} from 'node:stream';

export function publicIpv4(address:string):boolean {
  if(isIP(address)!==4)return false;
  const [a,b,c]=address.split('.').map(Number) as [number,number,number,number];
  return !(a===0||a===10||a===127||a>=224||a===100&&b>=64&&b<=127||a===169&&b===254||
    a===172&&b>=16&&b<=31||a===192&&(b===168||b===0||b===88&&c===99)||
    a===198&&(b===18||b===19||b===51&&c===100)||a===203&&b===0&&c===113);
}
function hostname(host:string):boolean {
  return host.length<=253 && host===host.toLowerCase() && isIP(host)===0 && host.includes('.') &&
    host.split('.').every(label=>/^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/.test(label));
}
export function policyHosts(input:unknown):ReadonlySet<string> {
  if(!Array.isArray(input)||input.length===0||input.length>64||input.some(h=>typeof h!=='string'||!hostname(h)))throw new Error('invalid_proxy_policy');
  const allowed=new Set<string>(input);
  if(allowed.size!==input.length)throw new Error('invalid_proxy_policy');
  return allowed;
}
export function targetHost(authority:string,allowed:ReadonlySet<string>):string|undefined {
  // Only canonical DNS authority and fixed TLS port. No URL normalization that
  // could silently reinterpret credentials, IP encodings, paths or backslashes.
  const match=/^([a-z0-9.-]+):443$/.exec(authority);
  return match && hostname(match[1]!) && allowed.has(match[1]!) ? match[1] : undefined;
}
interface Dependencies {
  lookup(host:string):Promise<Array<{address:string}>>;
  connect(address:string):Socket;
}
const production:Dependencies={
  lookup:host=>dnsLookup(host,{family:4,all:true,verbatim:true}),
  connect:address=>tcpConnect({host:address,port:443,family:4}),
};
export function createEgressProxy(hosts:ReadonlySet<string>,dependencies:Dependencies=production){
  const sockets=new Set<Duplex>();
  const server=createServer({maxHeaderSize:8192,headersTimeout:5000,requestTimeout:5000},(_req,res)=>{
    res.writeHead(405,{'Connection':'close','Content-Length':'0'});res.end();
  });
  server.maxConnections=32;
  server.on('connection',socket=>{
    sockets.add(socket);socket.once('close',()=>sockets.delete(socket));
    socket.setTimeout(5000,()=>socket.destroy());
    socket.on('error',()=>{});
  });
  server.on('clientError',(_error,socket)=>socket.destroy());
  server.on('connect',(request,socket,head)=>{
    if(!(socket instanceof Socket)){socket.destroy();return;}
    const host=targetHost(request.url??'',hosts);
    const deny=(code:number)=>socket.end(`HTTP/1.1 ${code} ${code===403?'Forbidden':'Bad Gateway'}\r\nConnection: close\r\nContent-Length: 0\r\n\r\n`);
    if(!host||head.length>65536){deny(403);return;}
    socket.pause();
    void (async()=>{
      let timer:NodeJS.Timeout|undefined;
      const timeout=new Promise<never>((_,reject)=>{timer=setTimeout(()=>reject(new Error('lookup_timeout')),5000);});
      let addresses:Array<{address:string}>;
      try {addresses=await Promise.race([dependencies.lookup(host),timeout]);}
      catch {if(!socket.destroyed)deny(502);return;}
      finally{clearTimeout(timer);}
      if(socket.destroyed)return;
      // Validate every answer and pin one numeric IP; never ask the connector
      // to resolve the hostname again (DNS rebinding must not bypass policy).
      if(addresses.length===0||addresses.length>32||addresses.some(a=>!publicIpv4(a.address))){deny(403);return;}
      let upstream:Socket;
      try{upstream=dependencies.connect(addresses[0]!.address);}catch{deny(502);return;}
      sockets.add(upstream);
      const deadline=setTimeout(()=>{socket.destroy();upstream.destroy();},15*60*1000);
      deadline.unref();
      upstream.setTimeout(5000,()=>upstream.destroy());
      let connected=false;
      socket.once('close',()=>upstream.destroy());
      upstream.once('close',()=>{clearTimeout(deadline);sockets.delete(upstream);socket.destroy();});
      upstream.on('error',()=>{if(!connected&&!socket.destroyed)deny(502);else socket.destroy();});
      upstream.once('connect',()=>{
        connected=true;
        if(socket.destroyed){upstream.destroy();return;}
        upstream.setTimeout(30000,()=>upstream.destroy());
        socket.setTimeout(30000,()=>socket.destroy());
        socket.write('HTTP/1.1 200 Connection Established\r\n\r\n');
        if(head.length)upstream.write(head);
        socket.pipe(upstream);upstream.pipe(socket);socket.resume();
      });
    })().catch(()=>socket.destroy());
  });
  return {server,close:async()=>{
    for(const socket of sockets)socket.destroy();
    await new Promise<void>((resolve,reject)=>server.close(error=>error?reject(error):resolve()));
  }};
}
