import { execFile, spawn, type ChildProcessWithoutNullStreams } from 'node:child_process';
import { promisify } from 'node:util';
import { promises as fs } from 'node:fs';
import { isAbsolute } from 'node:path';
import { isIP } from 'node:net';
import { policyHosts } from './egress-proxy.js';
import { milkieCommit } from './channel.js';

const exec = promisify(execFile);
const entry = '/opt/atelier/adapter/dist/src/';
interface ResourceOptions {
  workspaceId: string; ownershipToken: string; engineId: string; image: string; hosts: string[];
}
export interface Isolation extends ResourceOptions {
  runId: string;
  nativeDirectory: string; ledgerDirectory: string; configDirectory: string; skillDirectory: string;
}
export interface LoginIsolation extends ResourceOptions { loginId:string; runtime:'pi'|'grok-cli'; configDirectory:string; model?:string; }
export interface Containers { execution: ChildProcessWithoutNullStreams; proxy: ChildProcessWithoutNullStreams; }
const invalid = () => new Error('cli_isolation_invalid');
export function parseIsolation(value: unknown): Isolation {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw invalid();
  const p = value as Record<string, unknown>;
  const fields = ['runId','workspaceId','ownershipToken','engineId','image','nativeDirectory','ledgerDirectory','configDirectory','skillDirectory','hosts'];
  if (Object.keys(p).sort().join(',') !== fields.sort().join(',')) throw invalid();
  for (const key of ['runId','workspaceId','ownershipToken']) if (typeof p[key] !== 'string' || !/^[0-9a-f-]{36}$/.test(p[key] as string)) throw invalid();
  if (typeof p.engineId !== 'string' || !/^[a-zA-Z0-9:-]{1,128}$/.test(p.engineId)) throw invalid();
  if (typeof p.image !== 'string' || !/^(?:[a-zA-Z0-9./:_-]+@)?sha256:[a-f0-9]{64}$/.test(p.image)) throw invalid();
  for (const key of ['nativeDirectory','ledgerDirectory','configDirectory','skillDirectory']) {
    if (typeof p[key] !== 'string' || !isAbsolute(p[key] as string) || /[,\0\r\n]/.test(p[key] as string)) throw invalid();
  }
  policyHosts(p.hosts);
  return p as unknown as Isolation;
}
export function parseLoginIsolation(value:unknown):LoginIsolation {
  if(!value||typeof value!=='object'||Array.isArray(value))throw invalid();
  const p=value as Record<string,unknown>;
  const keys=['loginId','workspaceId','ownershipToken','engineId','image','hosts','runtime','configDirectory','model'];
  if(Object.keys(p).some(key=>!keys.includes(key))||!['pi','grok-cli'].includes(p.runtime as string)
    ||(p.model!==undefined&&(typeof p.model!=='string'||!p.model.trim()||p.model.startsWith('-')||p.model.length>256)))throw invalid();
  parseIsolation({runId:p.loginId,workspaceId:p.workspaceId,ownershipToken:p.ownershipToken,engineId:p.engineId,image:p.image,hosts:p.hosts,
    nativeDirectory:p.configDirectory,ledgerDirectory:p.configDirectory,configDirectory:p.configDirectory,skillDirectory:p.configDirectory});
  return p as unknown as LoginIsolation;
}
function environment(): NodeJS.ProcessEnv {
  const env: NodeJS.ProcessEnv = {};
  for (const key of ['PATH','HOME','DOCKER_HOST','DOCKER_CONTEXT','DOCKER_CONFIG']) if (process.env[key]) env[key] = process.env[key];
  return env;
}
async function docker(args: string[], signal: AbortSignal): Promise<string> {
  try { return (await exec('docker', args, { env: environment(), timeout: 15000, maxBuffer: 128 * 1024, signal })).stdout.trim(); }
  catch {
    const verb=['network','image','container'].includes(args[0]??'')?args.slice(0,2).join('_'):args[0];
    throw new Error(`cli_docker_${verb}_failed`);
  }
}
function attach(name: string): ChildProcessWithoutNullStreams {
  const child = spawn('docker',['start','-ai',name],{env:environment(),stdio:['pipe','pipe','pipe']});
  child.stderr.resume();
  child.stdin.on('error',()=>{});
  return child;
}
function ready(child: ChildProcessWithoutNullStreams, signal: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    let buffer = '';
    const finish = (error?: Error) => {
      clearTimeout(timer); child.off('exit', exit); child.off('error', exit); child.stdout.off('data', data); signal.removeEventListener('abort', abort);
      error ? reject(error) : resolve();
    };
    const abort = () => finish(new Error('cli_proxy_cancelled'));
    const exit = () => finish(new Error('cli_proxy_exited'));
    const data = (chunk: Buffer) => {
      buffer += chunk.toString();
      if (buffer.length > 8192) { finish(new Error('cli_proxy_protocol_invalid')); return; }
      if (buffer.includes('\n')) {
        try { const value = JSON.parse(buffer); if (value.state !== 'listening' || value.port !== 3128) throw invalid(); finish(); }
        catch { finish(new Error('cli_proxy_protocol_invalid')); }
      }
    };
    const timer = setTimeout(()=>finish(new Error('cli_proxy_start_timeout')),10000);
    child.once('exit',exit); child.once('error',exit); child.stdout.on('data',data); signal.addEventListener('abort',abort,{once:true});
    if (signal.aborted) abort();
  });
}

/** The Rust core has already committed these names/labels and registered this
 * launcher's PID before calling us. It owns cleanup and verifies immutable IDs;
 * this helper never deletes resources or treats pipe closure as proof of stop. */
async function createProxy(o:ResourceOptions,id:string,kind:'run'|'login'|'probe',uid:number,gid:number,signal:AbortSignal) {
  if (await docker(['info','--format','{{.ID}}'],signal) !== o.engineId) throw new Error('cli_engine_changed');
  const image = JSON.parse(await docker(['image','inspect',o.image],signal))[0];
  if (image?.Config?.Labels?.['atelier.milkie'] !== milkieCommit || image?.Config?.Labels?.['atelier.adapter.protocol'] !== '2') throw new Error('cli_image_incompatible');
  const executionName = kind==='run'?`atelier-cli-${id}`:`atelier-${kind}-${id}`;
  const prefix=kind==='run'?'atelier':`atelier-${kind}`;
  const proxyName=`${prefix}-proxy-${id}`,inner=`${prefix}-inner-${id}`,outer=`${prefix}-outer-${id}`;
  const labels = ['--label',`atelier.workspace=${o.workspaceId}`,'--label',`atelier.${kind}=${id}`,'--label',`atelier.owner=${o.ownershipToken}`];
  const common = ['--pull=never','--user',`${uid}:${gid}`,'--read-only','--cap-drop','ALL','--security-opt','no-new-privileges','--sysctl','net.ipv6.conf.all.disable_ipv6=1'];
  await docker(['network','create','--internal','--driver','bridge','--ipv6=false','--opt','com.docker.network.bridge.inhibit_ipv4=true',...labels,inner],signal);
  await docker(['network','create','--driver','bridge','--ipv6=false',...labels,outer],signal);
  const network = JSON.parse(await docker(['network','inspect',inner],signal))[0];
  if (network?.Internal !== true || network?.EnableIPv6 !== false || network?.Options?.['com.docker.network.bridge.inhibit_ipv4'] !== 'true') throw new Error('cli_network_incompatible');
  await docker(['create','-i','--name',proxyName,'--network',inner,...labels,...common,'--cpus','1','--memory','128m','--pids-limit','32','--entrypoint','node',o.image,entry+'proxy-main.js'],signal);
  await docker(['network','connect',outer,proxyName],signal);
  const proxy = attach(proxyName);
  try {
    const listening = ready(proxy,signal);
    // Attach errors may arrive while inspect waits; keep the rejection handled.
    void listening.catch(()=>{});
    let address = '';
    for (let attempt=0;attempt<80&&!address;attempt++) {
      const state = JSON.parse(await docker(['container','inspect',proxyName],signal))[0];
      address = state?.NetworkSettings?.Networks?.[inner]?.IPAddress ?? '';
      if (!address) await new Promise(resolve=>setTimeout(resolve,25));
    }
    if (isIP(address)!==4) throw new Error('cli_proxy_address_missing');
    proxy.stdin.write(JSON.stringify({listenHost:address,hosts:o.hosts})+'\n');
    await listening;
    return {proxy,address,executionName,inner,labels,common};
  } catch(error) {proxy.stdin.destroy();proxy.kill();throw error;}
}
export async function launchContainers(options: Isolation, signal: AbortSignal, kind:'run'|'probe'='run'): Promise<Containers> {
  const o = parseIsolation(options);
  const directories = [o.nativeDirectory,o.ledgerDirectory,o.configDirectory,o.skillDirectory];
  const real = await Promise.all(directories.map(path=>fs.realpath(path)));
  if(real.some(path=>/[,\0\r\n]/.test(path)))throw invalid();
  if (new Set(real).size !== real.length || real.some((path,i)=>real.some((other,j)=>i!==j&&path.startsWith(other+'/')))) throw invalid();
  const metadata = await Promise.all(directories.map(path=>fs.lstat(path)));
  const uid = metadata[0]!.uid, gid = metadata[0]!.gid;
  if (uid === 0 || metadata.some(stat=>!stat.isDirectory()||stat.isSymbolicLink()||(stat.mode&0o077)!==0||stat.uid!==uid||stat.gid!==gid)) throw invalid();
  const {proxy,address,executionName,inner,labels,common}=await createProxy(o,o.runId,kind,uid,gid,signal);
  let execution: ChildProcessWithoutNullStreams | undefined;
  try {
    const mount = (source: string, target: string, readonly = false) => ['--mount',`type=bind,source=${source},target=${target}${readonly?',readonly':''}`];
    await docker(['create','-i','--name',executionName,'--network',inner,...labels,...common,'--cpus','2','--memory','2g','--pids-limit','128',
      '--tmpfs',`/tmp:rw,noexec,nosuid,size=67108864,uid=${uid},gid=${gid},mode=700`,
      ...mount(real[0]!,'/state/native'),...mount(real[1]!,'/state/ledger'),...mount(real[2]!,'/config'),...mount(real[3]!,'/skill',true),
      '--env',`HTTP_PROXY=http://${address}:3128`,'--env',`HTTPS_PROXY=http://${address}:3128`,'--env','NODE_USE_ENV_PROXY=1','--env','LOG_LEVEL=silent',
      '--entrypoint','node',o.image,entry+(kind==='probe'?'cli-probe-main.js':'main.js')],signal);
    const state = JSON.parse(await docker(['container','inspect',executionName],signal))[0];
    if (state?.HostConfig?.Privileged !== false || state?.HostConfig?.ReadonlyRootfs !== true
      || state?.Config?.User !== `${uid}:${gid}` || Object.keys(state?.NetworkSettings?.Networks??{}).join(',')!==inner) throw new Error('cli_container_incompatible');
    execution = attach(executionName);
    return {execution,proxy};
  } catch (error) {
    execution?.stdin.destroy(); execution?.kill(); proxy.stdin.destroy(); proxy.kill();
    throw error;
  }
}

/** Creates a TTY login container, but does not attach to the user's terminal.
 * Rust owns the foreground Docker attach and keeps our private control pipe. */
export async function prepareLoginContainer(options:LoginIsolation,signal:AbortSignal):Promise<{proxy:ChildProcessWithoutNullStreams;containerName:string}> {
  const o=parseLoginIsolation(options);
  const metadata=await fs.lstat(o.configDirectory);
  if(!metadata.isDirectory()||metadata.isSymbolicLink()||(metadata.mode&0o077)!==0||metadata.uid===0)throw invalid();
  const config=await fs.realpath(o.configDirectory);
  if(/[,\0\r\n]/.test(config))throw invalid();
  const {uid,gid}=metadata;
  const {proxy,address,executionName,inner,labels,common}=await createProxy(o,o.loginId,'login',uid,gid,signal);
  try {
    const command=o.runtime==='grok-cli'?['grok','login','--device-auth']:
      ['pi','--no-tools','--no-builtin-tools','--no-extensions','--no-skills','--no-prompt-templates','--no-themes','--no-context-files','--offline',...(o.model?['--model',o.model]:[])];
    await docker(['create','-it','--name',executionName,'--network',inner,...labels,...common,'--cpus','2','--memory','2g','--pids-limit','128',
      '--tmpfs',`/tmp:rw,noexec,nosuid,size=67108864,uid=${uid},gid=${gid},mode=700`,'--workdir','/tmp',
      '--mount',`type=bind,source=${config},target=/config`,
      '--env','HOME=/config','--env','GROK_HOME=/config','--env','GROK_LEADER_SOCKET=/config/leader.sock','--env','PI_CODING_AGENT_DIR=/config',
      '--env','PI_CODING_AGENT_SESSION_DIR=/tmp/sessions','--env','TERM=xterm-256color','--env','LOG_LEVEL=silent',
      '--env',`HTTP_PROXY=http://${address}:3128`,'--env',`HTTPS_PROXY=http://${address}:3128`,'--env','NODE_USE_ENV_PROXY=1',
      '--entrypoint',command[0]!,o.image,...command.slice(1)],signal);
    if(proxy.exitCode!==null||signal.aborted)throw new Error('cli_login_proxy_exited');
    const state=JSON.parse(await docker(['container','inspect',executionName],signal))[0];
    const binds=state?.Mounts?.filter((m:{Type:string})=>m.Type==='bind');
    if(state?.Config?.User!==`${uid}:${gid}`||state?.Config?.Tty!==true||state?.HostConfig?.ReadonlyRootfs!==true||state?.HostConfig?.Privileged!==false
      ||Object.keys(state?.NetworkSettings?.Networks??{}).join(',')!==inner||binds?.length!==1||binds[0].Destination!=='/config'||binds[0].Source!==config||binds[0].RW!==true)throw new Error('cli_login_container_incompatible');
    return {proxy,containerName:executionName};
  }catch(error){proxy.stdin.destroy();proxy.kill();throw error;}
}
