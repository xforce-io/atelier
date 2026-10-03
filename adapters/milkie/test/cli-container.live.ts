/** Real Docker + production launcher/SDK; the CLI wire peer is a fixture.
 * This is isolation integration evidence, not native model acceptance. */
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { execFile, spawn, type ChildProcessWithoutNullStreams } from 'node:child_process';
import { promisify } from 'node:util';
import { mkdir, mkdtemp, readFile, writeFile, cp, rm } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { once } from 'node:events';
import { createInterface } from 'node:readline';
import { randomUUID, createHash } from 'node:crypto';
import { milkieCommit } from '../src/channel.js';

const exec = promisify(execFile);
async function docker(...args:string[]) { return (await exec('docker',args,{timeout:60000,maxBuffer:128*1024})).stdout.trim(); }
test('production launcher runs private CLI tools under real container limits and owns every resource', {timeout:120000}, async () => {
  const prepared=JSON.parse(await readFile(resolve('.cache/cli-image.json'),'utf8'));
  assert.match(prepared.image,/^sha256:[a-f0-9]{64}$/);
  // Colima shares the workspace, not macOS /private/var/folders temp paths.
  const root=await mkdtemp(resolve('.cache/cli-container-'));
  const runId=randomUUID(),workspaceId=randomUUID(),ownershipToken=randomUUID();
  const execution=`atelier-cli-${runId}`,proxy=`atelier-proxy-${runId}`,inner=`atelier-inner-${runId}`,outer=`atelier-outer-${runId}`;
  let child:ChildProcessWithoutNullStreams|undefined;
  let closed:Promise<unknown[]>|undefined;
  try {
    // Explicit test-only derivative: real SDK and launcher, no account/model.
    const build=join(root,'image');await mkdir(build,{mode:0o700});
    const baseTag=`atelier-cli-base:${prepared.image.slice(7)}`;
    await docker('tag',prepared.image,baseTag);
    await cp(resolve('test/fixtures/pi.cjs'),join(build,'pi.cjs'));
    await writeFile(join(build,'Dockerfile'),`FROM ${baseTag}\nCOPY pi.cjs /opt/fixture-pi.cjs\nRUN rm /usr/local/bin/pi && ln -s /opt/fixture-pi.cjs /usr/local/bin/pi && chmod 755 /opt/fixture-pi.cjs\nLABEL atelier.test.runtime="protocol-fixture"\n`);
    await docker('build','--iidfile',join(build,'id'),build);
    const image=(await readFile(join(build,'id'),'utf8')).trim();
    for(const name of ['native','ledger','config','skill'])await mkdir(join(root,name),{mode:0o700});
    await writeFile(join(root,'config','auth.json'),'{"fixture":true}',{mode:0o600});
    const skill='只查询当前任务；不得调用其它工具。';
    await writeFile(join(root,'skill','SKILL.md'),skill,{mode:0o600});
    child=spawn(process.execPath,[resolve('dist/src/cli-container-main.js')],{stdio:['pipe','pipe','pipe']});
    let diagnostic='';child.stderr.on('data',chunk=>diagnostic+=chunk);child.stdin.on('error',()=>{});closed=once(child,'close');
    const scope={taskId:randomUUID(),runId,deliveryId:randomUUID()};
    let seq=0;
    const frame=(kind:string,requestId:string,payload:unknown)=>JSON.stringify({protocolVersion:1,...scope,seq:++seq,kind,requestId,payload})+'\n';
    const lines=createInterface({input:child.stdout})[Symbol.asyncIterator]();
    const next=async()=>{const item=await lines.next();assert.equal(item.done,false,diagnostic);return JSON.parse(item.value!);};
    const isolation={runId,workspaceId,ownershipToken,engineId:await docker('info','--format','{{.ID}}'),image,
      nativeDirectory:join(root,'native'),ledgerDirectory:join(root,'ledger'),configDirectory:join(root,'config'),skillDirectory:join(root,'skill'),hosts:['example.com']};
    // Pipelined bootstrap and hello must not lose the protocol remainder.
    child.stdin.write(JSON.stringify(isolation)+'\n'+frame('hello','hello',{milkieCommit,transport:'agent-cli',skillDigest:createHash('sha256').update(skill).digest('hex'),builtinToolsDisabled:true,stableToolCallIds:true,nativeCheckpoint:true,privateTools:true}));
    assert.equal((await next()).kind,'ready');
    const inspected=JSON.parse(await docker('inspect',execution))[0];
    assert.equal(inspected.HostConfig.ReadonlyRootfs,true);
    assert.equal(inspected.HostConfig.Privileged,false);
    assert.equal(inspected.HostConfig.PidsLimit,128);
    assert.equal(inspected.HostConfig.Memory,2*1024**3);
    assert.equal(inspected.HostConfig.NanoCpus,2e9);
    assert.notEqual(inspected.Config.User.split(':')[0],'0');
    assert.deepEqual(Object.keys(inspected.NetworkSettings.Networks),[inner]);
    assert.equal(inspected.Config.Labels['atelier.owner'],ownershipToken);
    assert.deepEqual(inspected.Mounts.filter((m:{Type:string})=>m.Type==='bind').map((m:{Destination:string})=>m.Destination).sort(),['/config','/skill','/state/ledger','/state/native']);
    assert.equal(inspected.Mounts.find((m:{Destination:string})=>m.Destination==='/skill').RW,false);
    child.stdin.write(frame('start','start',{workerId:randomUUID(),contextId:randomUUID(),configurationId:'configuration',purposeFamily:'coordinate',
      contextDirectory:'/state/native',ledgerDirectory:'/state/ledger',resume:false,goal:'查询任务',input:JSON.stringify({calls:[{name:'task_read',input:{}}]}),skill,
      tools:[{name:'task_read',description:'读取任务',inputSchema:{type:'object',properties:{},additionalProperties:false}}],connection:{runtime:'pi',configDir:'/config'}}));
    const request=await next();assert.equal(request.kind,'tool.request');assert.equal(request.payload.name,'task_read');
    child.stdin.write(frame('tool.result',request.requestId,{ok:true,taskId:scope.taskId}));
    const terminal=await next();assert.equal(terminal.payload.stopReason,'completed');
    assert.equal((await closed)[0],0);
    assert.equal(JSON.parse(await docker('inspect',execution))[0].State.Running,false);
    const evidence=resolve('../../.agents/verify-runs/1/cli-container-'+runId+'.json');
    await mkdir(resolve('../../.agents/verify-runs/1'),{recursive:true});
    await writeFile(evidence,JSON.stringify({kind:'development_integration',image,milkie:milkieCommit,privatePipe:true,hostTools:true,limits:true,skillReadonly:true,nativeCli:false,model:false},null,2));
    console.log(`evidence: ${evidence}`);
  } finally {
    child?.stdin.destroy();if(child?.exitCode===null){child.kill('SIGKILL');await closed;}
    for(const [kind,name] of [['container',execution],['container',proxy],['network',inner],['network',outer]]) {
      let found;try{found=JSON.parse(await docker(kind!,'inspect',name!))[0];}catch{continue;}
      const labels=kind==='container'?found.Config.Labels:found.Labels;
      assert.equal(labels['atelier.owner'],ownershipToken,'never delete a foreign test resource');
      await docker(kind!,'rm',...(kind==='container'?['-f']:[]),found.Id);
    }
    await rm(root,{recursive:true,force:true});
  }
});
