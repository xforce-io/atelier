import assert from 'node:assert/strict';
import { mkdtemp, readFile, writeFile, rm, mkdir, chmod } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

const image = process.argv[2];
assert.match(image ?? '', /^sha256:[0-9a-f]{64}$/, 'Use an immutable local image ID');
const here = dirname(fileURLToPath(import.meta.url));
const baseline = await readFile(resolve(here,'../../samples/tic-tac-toe/index.html'),'utf8');
const original = '[[0,1,2],[3,4,5],[6,7,8],[0,3,6],[1,4,7],[2,5,8]]';
assert.equal(baseline.split(original).length,2);
const fixed = baseline.replace(original, original.slice(0,-1) + ',[0,4,8],[2,4,6]]');
// Docker on macOS may not share the system temp directory. Keep this public
// synthetic fixture under the repository's ignored test-evidence directory.
const work = resolve(here,'../../.agents/verify-runs/1');
await mkdir(work,{ recursive:true });
const dir = await mkdtemp(join(work,'checker-candidate-'));
await chmod(dir,0o755);
const evidence = [];
try {
  // This is checker calibration, not an Agent-produced artifact or task verification.
  for (const [name,html] of [['known-defect',baseline],['test-only-reference',fixed]]) {
    await writeFile(join(dir,'index.html'),html,{ mode:0o644 });
    const containerName = `atelier-calibration-${process.pid}-${name}`;
    const result = spawnSync('docker',[
      'run','--rm','--name',containerName,'--network','none','--read-only',
      '--cap-drop','ALL','--security-opt','no-new-privileges',
      '--env','DEBUG=pw:browser',
      '--env','XDG_CACHE_HOME=/tmp/cache',
      '--cpus','1','--cpuset-cpus','0','--memory','512m','--pids-limit','64',
      '--tmpfs','/tmp:rw,nosuid,nodev,size=134217728,mode=1777',
      '--mount',`type=bind,source=${dir},target=/candidate,readonly`,image,
    ],{ encoding:'utf8',timeout:120000,maxBuffer:5*1024*1024 });
    if (result.error) {
      spawnSync('docker',['rm','-f',containerName],{ encoding:'utf8',timeout:15000 });
      throw result.error;
    }
    assert.ok(result.stdout.trim(),`No checker result: ${result.stderr.slice(-6000)}`);
    const report = JSON.parse(result.stdout);
    assert.equal(report.results.length,19);
    const failed = report.results.filter(test => test.status !== 'pass');
    if (name === 'known-defect') {
      assert.equal(result.status,1);
      assert.deepEqual(failed.map(test => test.name),['X-winning-line-6','X-winning-line-7','O-winning-line-6','O-winning-line-7']);
    } else {
      assert.equal(result.status,0,result.stderr);
      assert.equal(failed.length,0,JSON.stringify(failed));
    }
    evidence.push({ calibration:name, image, report });
  }
  const evidencePath=join(work,`tic-tac-toe-calibration-${Date.now()}.json`);
  await writeFile(evidencePath,JSON.stringify({ calibrationOnly:true, productAcceptance:false, evidence },null,2),{flag:'wx',mode:0o600});
  console.log(JSON.stringify({calibrationOnly:true,productAcceptance:false,evidencePath,results:evidence.map(item=>({name:item.calibration,passed:item.report.results.filter(test=>test.status==='pass').length,failed:item.report.results.filter(test=>test.status!=='pass').length}))}));
} finally { await rm(dir,{ recursive:true,force:true }); }
