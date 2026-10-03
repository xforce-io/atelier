/** Real management CLI, TTY, Docker and resource cleanup; explicitly fake
 * login peers. No user credential, native account, or model acceptance. */
import assert from 'node:assert/strict';
import {test} from 'node:test';
import {execFile,spawn} from 'node:child_process';
import {promisify} from 'node:util';
import {once} from 'node:events';
import {readFile,writeFile,mkdir,mkdtemp,cp,rm} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {randomUUID} from 'node:crypto';

const exec=promisify(execFile);
async function docker(...args:string[]) {return (await exec('docker',args,{timeout:60000,maxBuffer:128*1024})).stdout.trim();}
const binary=resolve('../../target/debug/atelier');
async function cli(workspace:string,...args:string[]) {
  const result=JSON.parse((await exec(binary,['--workspace',workspace,'--json',...args],{timeout:30000})).stdout);
  assert.equal(result.ok,true);return result.data;
}
async function tty(workspace:string,args:string[],cancel=false) {
  const child=spawn('python3',[resolve('test/fixtures/terminal_driver.py'),binary,'--workspace',workspace,...args],{stdio:['pipe','pipe','pipe']});
  const closed=once(child,'close');let output='';let interrupted=false;
  child.stdout.on('data',chunk=>{output+=chunk;if(cancel&&!interrupted&&output.includes('FIXTURE_LOGIN_WAITING')){interrupted=true;child.stdin.write('\x03');}});
  child.stderr.on('data',chunk=>output+=chunk);child.stdin.on('error',()=>{});
  const timer=setTimeout(()=>child.kill('SIGKILL'),45000);
  try {assert.equal((await closed)[0],0,output);if(cancel)assert.ok(interrupted,output);return output;}
  finally{clearTimeout(timer);child.stdin.destroy();child.kill();await closed;}
}
test('real connection login uses dedicated TTY containers, persists material generation and reclaims cancelled login',{timeout:180000,skip:process.platform!=='darwin'},async()=>{
  await exec('cargo',['build','--locked'],{cwd:resolve('../..'),timeout:120000});
  const prepared=JSON.parse(await readFile(resolve('.cache/cli-image.json'),'utf8'));
  const root=await mkdtemp(resolve('.cache/cli-login-'));const evidenceId=randomUUID();
  const observed:Array<unknown>=[];
  try {
    const build=join(root,'image');await mkdir(build,{mode:0o700});
    const tag=`atelier-cli-base:${prepared.image.slice(7)}`;await docker('tag',prepared.image,tag);
    await cp(resolve('test/fixtures/cli-login.cjs'),join(build,'login.cjs'));
    await writeFile(join(build,'Dockerfile'),`FROM ${tag}\nCOPY login.cjs /opt/fixture-login.cjs\nRUN rm /usr/local/bin/pi /usr/local/bin/grok && chmod 755 /opt/fixture-login.cjs && ln -s /opt/fixture-login.cjs /usr/local/bin/pi && ln -s /opt/fixture-login.cjs /usr/local/bin/grok\nLABEL atelier.test.runtime="login-fixture"\n`);
    await docker('build','--iidfile',join(build,'id'),build);const image=(await readFile(join(build,'id'),'utf8')).trim();
    for(const runtime of ['pi','grok-cli']) {
      const workspace=join(root,runtime);await cli(workspace,'workspace','init','--name','登录集成测试');
      const file=join(root,`${runtime}.json`);await writeFile(file,JSON.stringify({transport:'agent-cli',runtime,image,egress_hosts:['example.com']}));
      const connection=await cli(workspace,'--request-id','connection','connection','create','--name',runtime,'--file',file);
      const id=connection.connection.id;
      const worker=await cli(workspace,'--request-id','worker','worker','create','--name','专用成员','--connection',id);
      const prepare=await cli(workspace,'--request-id','prepare','connection','prepare',id,'--revision','1','--worker',worker.id);
      assert.equal(prepare.environment.state,'prepared');
      const args=['--request-id','login','connection','login',id,'--revision','1','--worker',worker.id];
      await assert.rejects(cli(workspace,...args)); // No implicit prompt or mutation without a user terminal.
      assert.deepEqual((await cli(workspace,'connection','show',id)).cliLogins,[]);
      const output=await tty(workspace,args);assert.match(output,/FIXTURE_LOGIN_SAVED/);assert.equal(output.includes('synthetic-login-material'),false);
      const saved=(await cli(workspace,'request','show','login')).login;
      assert.equal(saved.code,'login_material_saved_unchecked');assert.equal(saved.resourcesStopped,true);assert.equal(saved.generation,1);
      assert.deepEqual((await cli(workspace,...args)).login,saved); // Replays without a TTY, never reopens native login.
      const shown=await cli(workspace,'connection','show',id);
      assert.equal(shown.cliEnvironments[0].loginMaterialReady,true);assert.equal(shown.executionSupported,false);
      assert.equal(await docker('container','ls','--all','--filter',`label=atelier.login=${saved.id}`,'--format','{{.ID}}'),'');
      assert.equal(await docker('network','ls','--filter',`label=atelier.login=${saved.id}`,'--format','{{.ID}}'),'');
      const auth=join(workspace,'cli-environments',worker.execution_config,'login');
      await writeFile(join(auth,'fixture-wait'),'synthetic cancellation mode',{mode:0o600});
      const cancelArgs=['--request-id','cancel-login','connection','login',id,'--revision','1','--worker',worker.id];
      await tty(workspace,cancelArgs,true);
      const cancelled=(await cli(workspace,'request','show','cancel-login')).login;
      assert.equal(cancelled.code,'native_login_failed');assert.equal(cancelled.resourcesStopped,true);assert.equal(cancelled.generation,2);
      const after=await cli(workspace,'connection','show',id);assert.equal(after.cliEnvironments[0].loginMaterialReady,false);
      assert.equal(await docker('container','ls','--all','--filter',`label=atelier.login=${cancelled.id}`,'--format','{{.ID}}'),'');
      assert.equal(await docker('network','ls','--filter',`label=atelier.login=${cancelled.id}`,'--format','{{.ID}}'),'');
      assert.deepEqual(await cli(workspace,'task','list'),[]);
      observed.push({runtime,image,saved,cancelled,nativeLogin:false,model:false});
    }
    const evidence=resolve(`../../.agents/verify-runs/1/cli-login-${evidenceId}.json`);
    await mkdir(resolve('../../.agents/verify-runs/1'),{recursive:true});
    await writeFile(evidence,JSON.stringify({kind:'development_integration',observed},null,2));console.log(`evidence: ${evidence}`);
  }finally {
    // The Rust owner normally cleans everything. On test failure retain the
    // workspace and its ownership ledger for explicit runtime reconciliation.
    if(observed.length===2)await rm(root,{recursive:true,force:true});
    else console.log(`incomplete login fixture workspace retained: ${root}`);
  }
});
