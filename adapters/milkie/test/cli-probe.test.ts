import assert from 'node:assert/strict';
import { test } from 'node:test';
import { mkdtemp, mkdir, writeFile, symlink, chmod, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { randomUUID } from 'node:crypto';
import { ExecutionClient } from '@freemanxu/milkie';
import { parseCliDiagnostic, probeCli } from '../src/cli-probe.js';
import { spawn } from 'node:child_process';
import { once } from 'node:events';

test('CLI diagnostic cannot select commands, credentials, storage or flags',()=>{
  const options={runtime:'pi',model:null,challenge:randomUUID()};assert.equal(parseCliDiagnostic(options).runtime,'pi');
  for(const patch of [{runtime:'sh'},{model:'--version'},{challenge:'invalid'},{apiKey:'secret'},{configDir:'/host'},{command:'sh'}])assert.throws(()=>parseCliDiagnostic({...options,...patch}));
});
test('real CLI SDK probe requires exactly the matching tool call, not a successful text response',async()=>{
  const root=await mkdtemp(join(tmpdir(),'atelier-probe-'));
  try {
    await mkdir(join(root,'bin'));const script=resolve('test/fixtures/pi.cjs');await chmod(script,0o755);await symlink(script,join(root,'bin','pi'));
    for(const [model,expected] of [['fixture','cli_tool_roundtrip'],['fixture-no-tool','cli_tool_unverified'],['fixture-wrong-challenge','cli_native_failed']]){
      const own=join(root,model!);await mkdir(own,{mode:0o700});for(const name of ['cwd','config','sessions'])await mkdir(join(own,name),{mode:0o700});
      await writeFile(join(own,'config','auth.json'),'{"fixture":true}',{mode:0o600});
      const execution=new ExecutionClient({dataDir:join(own,'sdk'),connection:{contractVersion:2,legacyEnv:{},fields:{transport:'agent-cli',runtime:'pi',model:model!}},env:{PATH:`${join(root,'bin')}:${process.env.PATH}`}});
      const context=execution.createContext(join(own,'cwd'),{configDir:join(own,'config'),sessionDir:join(own,'sessions')});
      const result=await probeCli(execution,context.contextId,randomUUID(),new AbortController().signal);
      assert.equal(result.code,expected);assert.equal(result.state,model==='fixture'?'passed':'failed');
      assert.equal(JSON.stringify(result).includes('fixture completed'),false);
    }
  }finally{await rm(root,{recursive:true,force:true});}
});
test('probe cancellation before launch creates no native execution',async()=>{
  const controller=new AbortController();controller.abort();
  const unused=new Proxy({},{get(){throw new Error('must not launch');}}) as ExecutionClient;
  assert.deepEqual(await probeCli(unused,'unused',randomUUID(),controller.signal),{state:'inconclusive',code:'cancelled'});
});
test('real diagnostic launcher rejects mismatched ownership without echoing input',async()=>{
  const child=spawn(process.execPath,[resolve('dist/src/cli-probe-launcher.js'),'--atelier-probe',randomUUID()],{env:{PATH:''},stdio:['pipe','pipe','pipe']});
  const closed=once(child,'close');let output='';child.stdout.on('data',chunk=>output+=chunk);child.stderr.on('data',chunk=>output+=chunk);
  child.stdin.end(JSON.stringify({isolation:{runId:randomUUID(),workspaceId:randomUUID(),ownershipToken:randomUUID(),engineId:'engine',image:'sha256:'+'a'.repeat(64),nativeDirectory:'/native',ledgerDirectory:'/ledger',configDirectory:'/config',skillDirectory:'/skill',hosts:['example.com']},diagnostic:{runtime:'pi',model:null,challenge:randomUUID()}})+'\n');
  assert.equal((await closed)[0],1);assert.equal(output,'');
});
