import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { gunzipSync, gzipSync } from 'node:zlib';

// NPM 0.1.1 predates the required tool allowlist. Build an immutable committed
// source snapshot; never build or alter a neighboring developer's dirty tree.
const revision = 'a3c1af0e479c9307140e6335da0e55efcf8785cc';
const root = dirname(fileURLToPath(import.meta.url));
const source = process.argv[2];
if (!source) throw new Error('用法：node prepare-dependency.mjs <milkie Git 仓库路径>');
function run(command, args, options = {}) {
  const result = spawnSync(command, args, { stdio: ['ignore', 'pipe', 'pipe'], maxBuffer: 64 * 1024 * 1024, timeout: 300000, ...options });
  if (result.error || result.status !== 0) {
    process.stderr.write(result.stderr ?? '');
    throw new Error(`${command} 失败（${result.status ?? '未退出'}）`);
  }
  return result.stdout;
}
const actual = run('git', ['-C', resolve(source), 'rev-parse', `${revision}^{commit}`]).toString().trim();
if (actual !== revision) throw new Error('milkie 固定提交缺失');
mkdirSync(join(root, '.cache'), { recursive: true });
mkdirSync(join(root, 'vendor'), { recursive: true });
const build = mkdtempSync(join(root, '.cache', 'milkie-build-'));
try {
  const archive = run('git', ['-C', resolve(source), 'archive', '--format=tar', revision]);
  run('tar', ['-xf', '-', '-C', build], { input: archive, stdio: ['pipe', 'pipe', 'pipe'] });
  run('npm', ['ci', '--ignore-scripts', '--no-audit', '--no-fund'], { cwd: build });
  run('npm', ['run', 'build'], { cwd: build });
  const packed = JSON.parse(run('npm', ['pack', '--ignore-scripts', '--json'], { cwd: build }).toString())[0];
  // npm's tar bytes are reproducible, but compressed bytes differ between the
  // official Node build and Homebrew's zlib. Stored DEFLATE blocks preserve the
  // exact tar while producing a portable artifact for npm's integrity check.
  const tar = gunzipSync(readFileSync(join(build, packed.filename)));
  const artifact = gzipSync(tar, { level: 0 });
  const sha256 = createHash('sha256').update(artifact).digest('hex');
  const tarSha256 = createHash('sha256').update(tar).digest('hex');
  const filename = `milkie-${revision}.tgz`;
  writeFileSync(join(root, 'vendor', filename), artifact);
  writeFileSync(join(root, 'vendor', 'provenance.json'), JSON.stringify({ revision, filename, sha256, tarSha256, source: 'committed Git snapshot; local changes excluded' }, null, 2) + '\n');
  process.stdout.write(JSON.stringify({ revision, filename, sha256, tarSha256 }) + '\n');
} finally {
  rmSync(build, { recursive: true, force: true });
}
