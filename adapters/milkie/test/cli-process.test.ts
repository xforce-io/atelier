import assert from 'node:assert/strict';
import { test } from 'node:test';
import { mkdtemp, mkdir, writeFile, readFile, rm, chmod, symlink } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';
import { once } from 'node:events';
import { createHash } from 'node:crypto';
import { parseCliStart, prepareCliContext } from '../src/cli-process.js';
import { milkieCommit } from '../src/channel.js';

async function fixture() {
  const root = await mkdtemp(join(tmpdir(), 'atelier-cli-process-'));
  for (const name of ['context','ledger','config','bin']) await mkdir(join(root, name), { mode: 0o700 });
  const file = resolve('test/fixtures/pi.cjs');
  await chmod(file, 0o755); await symlink(file, join(root, 'bin', 'pi'));
  await writeFile(join(root, 'config', 'auth.json'), '{"fixture":true}', { mode: 0o600 });
  const start = parseCliStart({ workerId:'worker', contextId:'context', configurationId:'configuration', purposeFamily:'coordinate',
    contextDirectory:join(root,'context'), ledgerDirectory:join(root,'ledger'), resume:false,
    goal:'回复当前消息', input:JSON.stringify({calls:[{name:'task_read',input:{}}]}), skill:'只使用当前授予的工具',
    tools:[{name:'task_read',description:'读取当前任务',inputSchema:{type:'object',properties:{},additionalProperties:false}}],
    connection:{runtime:'pi',configDir:join(root,'config')},
  });
  return { root, start, cleanup: () => rm(root, { recursive:true, force:true }) };
}

test('CLI start rejects native policy, ambient credentials, unsupported runtime and overlapping stores', async () => {
  const f = await fixture();
  try {
    for (const connection of [{...f.start.connection,apiKey:'not-allowed'}, {...f.start.connection,toolPolicy:'standard'}, {...f.start.connection,runtime:'invented'}, {...f.start.connection,configDir:'relative'}]) {
      assert.throws(() => parseCliStart({...f.start,connection}));
    }
    await assert.rejects(prepareCliContext({...f.start,connection:{...f.start.connection,configDir:f.start.contextDirectory}}, {taskId:'task',runId:'run',deliveryId:'delivery'}), /must_be_separate/);
    const scope = { taskId:'task',runId:'run',deliveryId:'delivery' };
    const prepared = await prepareCliContext(f.start,scope);
    assert.ok(prepared.nativeContextId);
    await assert.rejects(prepareCliContext(f.start,scope), /already_exists/);
    await assert.rejects(prepareCliContext({...f.start,resume:true,workerId:'other'},scope), /binding_mismatch/);
    await assert.rejects(prepareCliContext({...f.start,resume:true},scope), /not_started/);
    const manifest = await readFile(join(f.start.contextDirectory,'binding.json'),'utf8');
    assert.equal(manifest.includes('auth.json'),false);
  } finally { await f.cleanup(); }
});

test('production CLI child serves private tools and resumes one native context across distinct deliveries', {timeout:20000}, async () => {
  const f = await fixture();
  try {
    const nativeSessions: string[] = [];
    for (let round=0; round<2; round++) {
      const child = spawn(process.execPath,[resolve('dist/src/main.js')],{env:{PATH:`${join(f.root,'bin')}:${process.env.PATH}`},stdio:['pipe','pipe','pipe']});
      const closed = once(child,'close');
      const lines = createInterface({input:child.stdout})[Symbol.asyncIterator]();
      const scope = {taskId:'task',runId:`run-${round}`,deliveryId:`delivery-${round}`};
      let seq = 0;
      const send = (kind:string,requestId:string,payload:unknown) => child.stdin.write(JSON.stringify({protocolVersion:1,...scope,seq:++seq,kind,requestId,payload})+'\n');
      const next = async () => { const value=await lines.next(); assert.equal(value.done,false); return JSON.parse(value.value!); };
      child.stdin.on('error',()=>{});
      try {
        send('hello','hello',{milkieCommit,transport:'agent-cli',skillDigest:createHash('sha256').update(f.start.skill).digest('hex'),builtinToolsDisabled:true,stableToolCallIds:true,nativeCheckpoint:true,privateTools:true});
        assert.equal((await next()).kind,'ready');
        send('start','start',{...f.start,resume:round>0});
        const request = await next();
        assert.equal(request.kind,'tool.request');
        assert.equal(request.payload.originatingRunId,scope.runId);
        assert.equal(request.payload.name,'task_read');
        send('tool.result',request.requestId,{ok:true,taskId:'task'});
        const terminal = await next();
        assert.equal(terminal.kind,'terminal');
        assert.equal(terminal.payload.stopReason,'completed');
        assert.equal((await closed)[0],0);
        const binding = JSON.parse(await readFile(join(f.start.contextDirectory,'binding.json'),'utf8'));
        const native = JSON.parse(await readFile(join(f.start.contextDirectory,'sdk','contexts',`${binding.nativeContextId}.json`),'utf8'));
        nativeSessions.push(native.nativeSessionId);
      } finally { if(child.exitCode===null) {child.kill('SIGKILL');await closed;} }
    }
    assert.equal(nativeSessions[0],nativeSessions[1]);
  } finally { await f.cleanup(); }
});
