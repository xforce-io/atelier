/** Actual Atelier service + SQLite + Docker + milkie. Login/CLI are explicit
 * protocol fixtures; this is integration proof, never native-model acceptance. */
import assert from 'node:assert/strict';
import {test} from 'node:test';
import {execFile} from 'node:child_process';
import {promisify} from 'node:util';
import {mkdir,mkdtemp,readFile,writeFile,cp,rm,readdir} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {randomUUID} from 'node:crypto';
const exec=promisify(execFile);
const binary=resolve('../../target/debug/atelier');
async function cli(workspace:string,...args:string[]){const value=JSON.parse((await exec(binary,['--workspace',workspace,'--json',...args],{timeout:30000})).stdout);assert.equal(value.ok,true);return value.data;}
async function docker(...args:string[]){return (await exec('docker',args,{timeout:60000,maxBuffer:128*1024})).stdout.trim();}
async function until<T>(read:()=>Promise<T>,ready:(v:T)=>boolean):Promise<T>{const deadline=Date.now()+30000;let value:T;do{value=await read();if(ready(value))return value;await new Promise(r=>setTimeout(r,150));}while(Date.now()<deadline);assert.fail(JSON.stringify(value));}

test('service dispatches CLI mailbox tools, resumes per-task sessions, and stops owned containers',{timeout:180000},async()=>{
  await exec('cargo',['build','--locked'],{cwd:resolve('../..'),timeout:120000});
  const prepared=JSON.parse(await readFile(resolve('.cache/cli-image.json'),'utf8'));
  const root=await mkdtemp(resolve('.cache/cli-runtime-'));const workspace=join(root,'workspace');
  let complete=false;let service=false;
  try {
    const build=join(root,'image');await mkdir(build,{mode:0o700});
    await cp(resolve('test/fixtures/pi.cjs'),join(build,'pi.cjs'));
    await cp(resolve('test/fixtures/cli-login.cjs'),join(build,'login.cjs'));
    await writeFile(join(build,'entry.cjs'),"#!/usr/bin/env node\nrequire(process.argv.includes('--offline')?'/opt/fixture-login.cjs':'/opt/fixture-pi.cjs');\n");
    const tag=`atelier-cli-base:${prepared.image.slice(7)}`;await docker('tag',prepared.image,tag);
    await writeFile(join(build,'Dockerfile'),`FROM ${tag}\nCOPY pi.cjs /opt/fixture-pi.cjs\nCOPY login.cjs /opt/fixture-login.cjs\nCOPY entry.cjs /opt/fixture-entry.cjs\nRUN rm /usr/local/bin/pi && chmod 755 /opt/fixture-entry.cjs && ln -s /opt/fixture-entry.cjs /usr/local/bin/pi\nLABEL atelier.test.runtime="service-fixture"\n`);
    await docker('build','--iidfile',join(build,'id'),build);const image=(await readFile(join(build,'id'),'utf8')).trim();
    const initialized=await cli(workspace,'workspace','init','--name','运行集成测试');const human=initialized.self.id;
    const spec=join(root,'connection.json');await writeFile(spec,JSON.stringify({transport:'agent-cli',runtime:'pi',image,egress_hosts:['example.com']}));
    const connection=(await cli(workspace,'--request-id','connection','connection','create','--name','Pi fixture','--file',spec)).connection;
    const worker=await cli(workspace,'--request-id','worker','worker','create','--name','团队负责人','--connection',connection.id);
    await cli(workspace,'--request-id','prepare','connection','prepare',connection.id,'--revision','1','--worker',worker.id);
    await exec('python3',[resolve('test/fixtures/terminal_driver.py'),binary,'--workspace',workspace,'--request-id','login','connection','login',connection.id,'--revision','1','--worker',worker.id],{timeout:30000});
    assert.equal((await cli(workspace,'request','show','login')).login.code,'login_material_saved_unchecked');
    const team=await cli(workspace,'--request-id','team','team','create','--name','测试团队','--members',`${human},${worker.id}`,'--leader',worker.id);
    await cli(workspace,'--request-id','grants','team','permissions','update',team.id,'--revision','1','--grant',`${worker.id}:task.communicate`,'--grant',`${worker.id}:task.arrange`,'--grant',`${human}:task.communicate`);
    const first=await cli(workspace,'--request-id','task','task','create','--team',team.id,'--goal','真实服务协议测试');
    await cli(workspace,'runtime','start');service=true;
    const receipts=await until(()=>cli(workspace,'mailbox','list','--worker',worker.id),v=>v.some((r:{status:string})=>r.status==='handled'));
    assert.equal(receipts.length,1);assert.equal((await cli(workspace,'task','show',first.task.id)).runs_used,1);
    const contexts=await readdir(join(workspace,'contexts'));assert.equal(contexts.length,1);
    const native=join(workspace,'contexts',contexts[0]!,'native');
    const results=JSON.parse(await readFile(join(native,'cwd','last-results.json'),'utf8'));
    assert.equal(results.length,2);assert.ok(results.every((r:{ok:boolean})=>r.ok),JSON.stringify(results));
    const binding=await readFile(join(native,'binding.json'),'utf8');
    const sessions=await readdir(join(native,'sessions'));
    // A new ordinary message must reuse this context but get a separate ledger.
    await cli(workspace,'--request-id','note','message','send','--task',first.task.id,'--recipient',worker.id,'--kind','work.note','--body','继续核对');
    await until(()=>cli(workspace,'mailbox','list','--worker',worker.id),v=>v.length===2&&v.every((r:{status:string})=>r.status==='handled'));
    assert.equal(await readFile(join(native,'binding.json'),'utf8'),binding);
    assert.deepEqual(await readdir(join(native,'sessions')),sessions);
    assert.equal((await readdir(join(workspace,'contexts',contexts[0]!,'ledger'))).length,2);
    // Same Worker, different Task: independent session mount with shared login.
    const second=await cli(workspace,'--request-id','second-task','task','create','--team',team.id,'--goal','另一个任务');
    await until(()=>cli(workspace,'task','show',second.task.id),v=>v.runs_used===1);
    await until(()=>cli(workspace,'mailbox','list','--worker',worker.id),v=>v.length===3&&v.every((r:{status:string})=>r.status==='handled'));
    assert.equal((await readdir(join(workspace,'contexts'))).length,2);
    // Hold one native fixture turn, then stop through the real service entry.
    await cli(workspace,'--request-id','hold','message','send','--task',first.task.id,'--recipient',worker.id,'--kind','work.note','--body','fixture-hold');
    await until(()=>cli(workspace,'task','show',first.task.id),v=>v.runs_used===3);
    await until(()=>docker('container','ls','--filter',`label=atelier.workspace=${initialized.id}`,'--format','{{.Names}}'),v=>v.includes('atelier-cli-'));
    const stopped=await cli(workspace,'runtime','stop');assert.equal(stopped.state,'stopped');service=false;
    assert.equal(await docker('container','ls','--all','--filter',`label=atelier.workspace=${initialized.id}`,'--format','{{.ID}}'),'');
    assert.equal(await docker('network','ls','--filter',`label=atelier.workspace=${initialized.id}`,'--format','{{.ID}}'),'');
    const login=(await cli(workspace,'connection','show',connection.id)).cliEnvironments[0];assert.equal(login.loginGeneration,1);assert.equal(login.loginMaterialReady,true);
    const evidence=resolve(`../../.agents/verify-runs/1/cli-runtime-${randomUUID()}.json`);
    await writeFile(evidence,JSON.stringify({kind:'development_integration',image,actualService:true,tools:true,contextReuse:true,taskIsolation:true,stopCleanup:true,nativeCli:false,model:false},null,2));
    console.log(`evidence: ${evidence}`);complete=true;
  } finally {
    if(service)await cli(workspace,'runtime','stop').catch(()=>{});
    if(complete)await rm(root,{recursive:true,force:true});
    else console.log(`incomplete runtime fixture workspace retained: ${root}`);
  }
});
