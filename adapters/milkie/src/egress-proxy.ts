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
export interface UpstreamProxy { address:string; port:number; }
export function parseUpstreamProxy(value:unknown):UpstreamProxy {
  if(!value||typeof value!=='object'||Array.isArray(value))throw new Error('invalid_upstream_proxy');
  const p=value as Record<string,unknown>;
  if(Object.keys(p).sort().join(',')!=='address,port'||typeof p.address!=='string'||isIP(p.address)!==4
    ||typeof p.port!=='number'||!Number.isInteger(p.port)||p.port<1||p.port>65535)throw new Error('invalid_upstream_proxy');
  const [a,b,c]=p.address.split('.').map(Number) as [number,number,number,number];
  if(a===0||a===127||a>=224||a===169&&b===254||a===192&&(b===0||b===88&&c===99)
    ||a===198&&(b===18||b===19||b===51&&c===100)||a===203&&b===0&&c===113)throw new Error('invalid_upstream_proxy');
  return p as unknown as UpstreamProxy;
}
interface Dependencies {
  lookup(host:string):Promise<Array<{address:string}>>;
  connect(address:string,upstream?:UpstreamProxy):Socket;
}
const production:Dependencies={
  lookup:host=>dnsLookup(host,{family:4,all:true,verbatim:true}),
  connect:(address,upstream)=>tcpConnect({host:upstream?.address??address,port:upstream?.port??443,family:4}),
};
/** CONNECT the already-vetted numeric target, never resolve it at the next hop.
 * No authentication headers, redirects, ambient proxy settings, or fallback. */
function tunnel(socket:Socket,address:string):Promise<void> {
  return new Promise((resolve,reject)=>{
    let buffer=Buffer.alloc(0);
    const fail=()=>finish(new Error('upstream_tunnel_failed'));
    const finish=(error?:Error)=>{socket.off('data',data);socket.off('error',fail);socket.off('close',fail);error?reject(error):resolve();};
    const data=(chunk:Buffer)=>{
      buffer=Buffer.concat([buffer,chunk]);
      const end=buffer.indexOf('\r\n\r\n');
      if(buffer.length>8192){fail();return;}
      if(end<0)return;
      if(!/^HTTP\/1\.[01] 200(?: [^\r\n]*)?\r\n/.test(buffer.subarray(0,end+2).toString('ascii'))){fail();return;}
      socket.pause();if(buffer.length>end+4)socket.unshift(buffer.subarray(end+4));finish();
    };
    socket.on('data',data);socket.once('error',fail);socket.once('close',fail);
    socket.write(`CONNECT ${address}:443 HTTP/1.1\r\nHost: ${address}:443\r\n\r\n`);
  });
}
export function createEgressProxy(hosts:ReadonlySet<string>,dependencies:Dependencies=production,upstreamProxy?:UpstreamProxy){
  const route=upstreamProxy===undefined?undefined:parseUpstreamProxy(upstreamProxy);
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
    const deny=(code:number)=>{if(!socket.destroyed&&!socket.writableEnded)socket.end(`HTTP/1.1 ${code} ${code===403?'Forbidden':'Bad Gateway'}\r\nConnection: close\r\nContent-Length: 0\r\n\r\n`);};
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
      const address=addresses[0]!.address;
      try{upstream=dependencies.connect(address,route);}catch{deny(502);return;}
      sockets.add(upstream);
      const deadline=setTimeout(()=>{socket.destroy();upstream.destroy();},15*60*1000);
      deadline.unref();
      upstream.setTimeout(5000,()=>upstream.destroy());
      let connected=false;
      socket.once('close',()=>upstream.destroy());
      upstream.once('close',()=>{clearTimeout(deadline);sockets.delete(upstream);if(connected)socket.destroy();else deny(502);});
      upstream.on('error',()=>{if(!connected&&!socket.destroyed)deny(502);else socket.destroy();});
      upstream.once('connect',()=>{
        void (async()=>{
        if(route)await tunnel(upstream,address);
        connected=true;
        if(socket.destroyed){upstream.destroy();return;}
        upstream.setTimeout(30000,()=>upstream.destroy());
        socket.setTimeout(30000,()=>socket.destroy());
        socket.write('HTTP/1.1 200 Connection Established\r\n\r\n');
        if(head.length)upstream.write(head);
        socket.pipe(upstream);upstream.pipe(socket);socket.resume();
        })().catch(()=>{if(!socket.destroyed)deny(502);upstream.destroy();});
      });
    })().catch(()=>socket.destroy());
  });
  return {server,close:async()=>{
    for(const socket of sockets)socket.destroy();
    await new Promise<void>((resolve,reject)=>server.close(error=>error?reject(error):resolve()));
  }};
}
