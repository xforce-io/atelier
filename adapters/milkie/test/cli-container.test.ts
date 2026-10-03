import assert from 'node:assert/strict';
import { test } from 'node:test';
import { randomUUID } from 'node:crypto';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { resolve } from 'node:path';
import { parseIsolation, parseLoginIsolation } from '../src/cli-container.js';

test('isolation bootstrap cannot inject Docker options, mutable images or arbitrary egress',()=>{
  const value={runId:randomUUID(),workspaceId:randomUUID(),ownershipToken:randomUUID(),engineId:'engine',image:'sha256:'+'a'.repeat(64),nativeDirectory:'/native',ledgerDirectory:'/ledger',configDirectory:'/config',skillDirectory:'/skill',hosts:['example.com']};
  assert.equal(parseIsolation(value).runId,value.runId);
  for(const patch of [{image:'node:latest'},{hosts:['*']},{hosts:['127.0.0.1']},{nativeDirectory:'/source,target=/host'},{extraDockerArgs:['--privileged']},{runId:'../other'}]) assert.throws(()=>parseIsolation({...value,...patch}));
});

test('native login has a separate identity and cannot select host mounts or arbitrary command arguments',()=>{
  const value={loginId:randomUUID(),workspaceId:randomUUID(),ownershipToken:randomUUID(),engineId:'engine',image:'sha256:'+'a'.repeat(64),runtime:'pi',configDirectory:'/worker-login',hosts:['example.com']};
  assert.equal(parseLoginIsolation(value).runtime,'pi');
  for(const patch of [{runId:randomUUID()},{runtime:'bash'},{command:['sh']},{model:'--version'},{configDirectory:'/a,target=/host'},{hosts:['*']}])assert.throws(()=>parseLoginIsolation({...value,...patch}));
});
test('real launcher rejects malformed private bootstrap without echoing its contents',async()=>{
  const child=spawn(process.execPath,[resolve('dist/src/cli-container-main.js')],{stdio:['pipe','pipe','pipe']});
  const closed=once(child,'close');let output='';child.stdout.on('data',chunk=>output+=chunk);child.stderr.on('data',chunk=>output+=chunk);
  child.stdin.end('{"syntheticSecret":"DO-NOT-ECHO"}\n');
  assert.equal((await closed)[0],1);assert.equal(output.includes('DO-NOT-ECHO'),false);
  assert.match(output,/isolation launcher failed/);
});

test('launcher process marker must match the private resource binding before Docker creation',async()=>{
  const options={runId:randomUUID(),workspaceId:randomUUID(),ownershipToken:randomUUID(),engineId:'engine',image:'sha256:'+'a'.repeat(64),nativeDirectory:'/native',ledgerDirectory:'/ledger',configDirectory:'/config',skillDirectory:'/skill',hosts:['example.com']};
  const child=spawn(process.execPath,[resolve('dist/src/cli-container-main.js'),'--atelier-run',randomUUID()],{env:{PATH:''},stdio:['pipe','pipe','pipe']});
  const closed=once(child,'close');let output='';child.stdout.on('data',chunk=>output+=chunk);child.stderr.on('data',chunk=>output+=chunk);
  child.stdin.end(JSON.stringify(options)+'\n');
  assert.equal((await closed)[0],1);assert.match(output,/cli_launcher_run_mismatch/);
  assert.equal(output.includes(options.runId),false);
});
