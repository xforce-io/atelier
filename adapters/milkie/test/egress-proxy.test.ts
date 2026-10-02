import assert from 'node:assert/strict';
import {test} from 'node:test';
import {once} from 'node:events';
import {spawn} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import {createConnection,createServer,type Socket} from 'node:net';
import {createEgressProxy,policyHosts,publicIpv4,targetHost} from '../src/egress-proxy.js';

test('egress policy rejects wildcard, IP, private/reserved answers and authority ambiguity',()=>{
  const hosts=policyHosts(['api.example.com']);
  assert.equal(targetHost('api.example.com:443',hosts),'api.example.com');
  for(const value of ['api.example.com.evil:443','API.EXAMPLE.COM:443','api.example.com.:443','api.example.com:80','api.example.com:0443','user@api.example.com:443','api.example.com:443/path','api.example.com\\evil:443','api.example.com:443?x','api.example.com:443\r\nHost: evil','127.0.0.1:443','2130706433:443','[::1]:443'])assert.equal(targetHost(value,hosts),undefined,value);
  for(const values of [[],['*.example.com'],['127.0.0.1'],['example.com','example.com'],['localhost'],['a..com']])assert.throws(()=>policyHosts(values));
  for(const ip of ['0.1.2.3','10.1.2.3','100.64.0.1','127.0.0.1','169.254.169.254','172.31.0.1','192.168.1.1','192.0.2.3','192.88.99.1','198.18.0.1','198.51.100.1','203.0.113.1','224.0.0.1','255.255.255.255','::1','::ffff:8.8.8.8','0x7f000001'])assert.equal(publicIpv4(ip),false,ip);
  for(const ip of ['8.8.8.8','1.1.1.1','172.32.0.1','100.128.0.1'])assert.equal(publicIpv4(ip),true,ip);
});
function readHeader(socket:Socket):Promise<string>{return new Promise((resolve,reject)=>{
  let output='';socket.on('data',chunk=>{output+=chunk.toString();if(output.includes('\r\n\r\n'))resolve(output);});
  socket.once('error',reject);socket.once('end',()=>resolve(output));
});}
test('real proxy socket rejects before DNS, pins a vetted answer, and closes active tunnels',async()=>{
  const echo=createServer(socket=>socket.pipe(socket));echo.listen(0,'127.0.0.1');await once(echo,'listening');
  const echoPort=(echo.address() as {port:number}).port;
  let lookups=0,connects=0;let answers=[{address:'8.8.8.8'}];
  const proxy=createEgressProxy(policyHosts(['api.example.com']),{
    async lookup(host){assert.equal(host,'api.example.com');lookups++;return answers;},
    connect(ip){assert.equal(ip,'8.8.8.8');connects++;return createConnection({host:'127.0.0.1',port:echoPort});},
  });
  proxy.server.listen(0,'127.0.0.1');await once(proxy.server,'listening');const port=(proxy.server.address() as {port:number}).port;
  async function request(authority:string,method='CONNECT'){
    const socket=createConnection({host:'127.0.0.1',port});await once(socket,'connect');
    const header=readHeader(socket);socket.write(`${method} ${authority} HTTP/1.1\r\nHost: ${authority}\r\n\r\n`);
    return {socket,header:await header};
  }
  try{
    let response=await request('other.example.com:443');assert.match(response.header,/403/);response.socket.destroy();assert.equal(lookups,0);
    response=await request('http://api.example.com/','GET');assert.match(response.header,/405/);response.socket.destroy();assert.equal(lookups,0);
    answers=[{address:'8.8.8.8'},{address:'127.0.0.1'}];response=await request('api.example.com:443');assert.match(response.header,/403/);response.socket.destroy();assert.equal(connects,0);
    answers=[{address:'8.8.8.8'}];response=await request('api.example.com:443');assert.match(response.header,/200/);assert.equal(connects,1);
    const echoed=once(response.socket,'data');response.socket.write('fixture-through-tunnel');assert.equal((await echoed)[0].toString(),'fixture-through-tunnel');
    const closed=once(response.socket,'close');await proxy.close();await closed;
    assert.equal(lookups,2,'one DNS resolution per permitted CONNECT, never re-resolve during dialing');
  }finally{
    if(proxy.server.listening)await proxy.close();await new Promise<void>(resolve=>echo.close(()=>resolve()));
  }
});


test('private proxy entry rejects malformed configuration without diagnostic disclosure',async()=>{
  for(const input of ['', JSON.stringify({listenHost:'203.0.113.5',hosts:['api.example.com']})+'\n', JSON.stringify({listenHost:'127.0.0.1',hosts:['*.example.com'],secret:'do-not-echo'})+'\n']) {
    const child=spawn(process.execPath,[fileURLToPath(new URL('../src/proxy-main.js',import.meta.url))],{stdio:['pipe','pipe','pipe']});
    let stdout='',stderr='';child.stdout.on('data',b=>stdout+=b);child.stderr.on('data',b=>stderr+=b);
    const done=once(child,'exit');child.stdin.end(input);const [code]=await done;
    assert.equal(code,1);assert.equal(stdout,'');assert.equal(stderr,'');
  }
});
