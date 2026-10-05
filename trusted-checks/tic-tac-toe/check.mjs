import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { createHash } from 'node:crypto';
import { chromium } from 'playwright';

// Trusted rules are independent of the candidate's code and winning-line list.
const lines = [[0,1,2],[3,4,5],[6,7,8],[0,3,6],[1,4,7],[2,5,8],[0,4,8],[2,4,6]];
const winner = cells => lines.findIndex(line => cells[line[0]] && line.every(i => cells[i] === cells[line[0]]));
function winningSequence(target, lineIndex) {
  function search(cells, moves) {
    const won = winner(cells);
    if (won >= 0) return won === lineIndex && cells[lines[won][0]] === target ? moves : undefined;
    if (moves.length >= (target === 'X' ? 5 : 6)) return;
    const player = moves.length % 2 ? 'O' : 'X';
    for (let cell = 0; cell < 9; cell++) {
      if (cells[cell] || (player === target) !== lines[lineIndex].includes(cell)) continue;
      const copy = [...cells]; copy[cell] = player;
      const result = search(copy, [...moves, cell]);
      if (result) return result;
    }
  }
  const result = search(Array(9).fill(''), []);
  assert.ok(result, `No legal sequence for ${target}, line ${lineIndex}`);
  return result;
}

const candidate = resolve(process.argv[2] ?? '/candidate', 'index.html');
const digest = createHash('sha256').update(await readFile(candidate)).digest('hex');
const results = [];
const browser = await chromium.launch({ headless: true, timeout: 15000, args: [
  '--disable-dev-shm-usage', '--single-process', '--no-zygote',
  '--disable-gpu', '--disable-software-rasterizer',
] });
try {
  const context = await browser.newContext({ offline: true, serviceWorkers: 'block' });
  await context.route(/^https?:/, route => route.abort());
  const page = await context.newPage();
  page.setDefaultTimeout(1500);
  const status = () => page.locator('[data-testid="status"]').textContent();
  const board = () => page.locator('[data-cell]').allTextContents();
  const click = cell => page.locator(`[data-cell="${cell}"]`).click({ force: true });
  async function scenario(name, check) {
    try {
      await page.goto(pathToFileURL(candidate).href);
      await check();
      results.push({ name, status: 'pass' });
    } catch (error) {
      results.push({ name, status: 'fail', reason: String(error.message).slice(0,2000) });
    }
  }
  await scenario('initial-turn-and-duplicate-click', async () => {
    assert.deepEqual(await board(), Array(9).fill(''));
    assert.equal(await status(), '轮到 X');
    await click(0); assert.equal(await status(), '轮到 O');
    await click(0); assert.equal(await status(), '轮到 O');
    assert.deepEqual(await board(), ['X','','','','','','','','']);
    await click(1); assert.equal(await status(), '轮到 X');
  });
  for (const player of ['X','O']) for (let line = 0; line < lines.length; line++) {
    await scenario(`${player}-winning-line-${line}`, async () => {
      const sequence = winningSequence(player,line);
      for (const cell of sequence) await click(cell);
      assert.equal(await status(), `${player} 获胜`);
      const before = await board();
      for (let cell = 0; cell < 9; cell++) await click(cell);
      assert.deepEqual(await board(), before);
      assert.equal(await status(), `${player} 获胜`);
      await page.locator('[data-action=restart]').click();
      assert.deepEqual(await board(), Array(9).fill(''));
      assert.equal(await status(), '轮到 X');
    });
  }
  await scenario('draw-and-reset', async () => {
    for (const cell of [0,1,2,4,3,5,7,6,8]) await click(cell);
    assert.equal(await status(), '平局');
    const before = await board();
    for (let cell = 0; cell < 9; cell++) await click(cell);
    assert.deepEqual(await board(), before);
    assert.equal(await status(), '平局');
    await page.locator('[data-action=restart]').click();
    assert.deepEqual(await board(), Array(9).fill(''));
    assert.equal(await status(), '轮到 X');
  });
  await scenario('reset-during-game-and-play-again', async () => {
    for (let round = 0; round < 3; round++) {
      await click(0); await click(4);
      await page.locator('[data-action=restart]').click();
      assert.deepEqual(await board(), Array(9).fill(''));
      assert.equal(await status(), '轮到 X');
    }
  });
} finally { await browser.close(); }
const report = { checkId: 'tic-tac-toe-browser-v1', candidateIndexSha256: digest, offline: true, results };
console.log(JSON.stringify(report));
if (results.some(result => result.status !== 'pass')) process.exitCode = 1;
