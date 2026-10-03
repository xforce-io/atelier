import { cpSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';

// Build only explicit application files and the supplied Linux CLI binary.
// Never send a workspace, home directory, login, or session as Docker context.
const root = dirname(fileURLToPath(import.meta.url));
const binary = process.argv[2];
if (process.argv.length !== 3) throw new Error('用法：node prepare-cli-image.mjs <Grok Linux arm64 二进制路径>');
function run(command, args, cwd = root) {
  const result = spawnSync(command, args, { cwd, stdio: 'inherit', timeout: 600000 });
  if (result.error || result.status !== 0) throw new Error('CLI 镜像准备失败');
}
run('npm', ['run','build']);
mkdirSync(join(root,'.cache'),{recursive:true});
const stage = mkdtempSync(join(root,'.cache','cli-image-'));
try {
  const pkg = JSON.parse(readFileSync(join(root,'package.json'),'utf8'));
  const dependency = pkg.dependencies['@freemanxu/milkie'].replace(/^file:/,'');
  if (!/^vendor\/milkie-[a-f0-9]{40}\.tgz$/.test(dependency)) throw new Error('milkie 依赖必须是固定快照');
  for (const name of ['package.json','package-lock.json']) cpSync(join(root,name),join(stage,name));
  mkdirSync(join(stage,'vendor'));cpSync(join(root,dependency),join(stage,dependency));
  cpSync(join(root,'dist','src'),join(stage,'dist','src'),{recursive:true});
  cpSync(join(root,'cli-image.Dockerfile'),join(stage,'Dockerfile'));
  cpSync(resolve(binary),join(stage,'grok'));
  const grokSha256=createHash('sha256').update(readFileSync(join(stage,'grok'))).digest('hex');
  const imageIdFile=join(stage,'image-id');
  run('docker',['build','--platform','linux/arm64','--iidfile',imageIdFile,'--build-arg',`GROK_SHA256=${grokSha256}`,stage]);
  const image=readFileSync(imageIdFile,'utf8').trim();
  if(!/^sha256:[a-f0-9]{64}$/.test(image))throw new Error('未取得固定镜像标识');
  const record={image,milkie:dependency.match(/[a-f0-9]{40}/)[0],grokSha256,piVersion:'0.85.1',platform:'linux/arm64',nativeAcceptance:'not_run'};
  writeFileSync(join(root,'.cache','cli-image.json'),JSON.stringify(record,null,2)+'\n');
  process.stdout.write(JSON.stringify(record)+'\n');
} finally {rmSync(stage,{recursive:true,force:true});}
