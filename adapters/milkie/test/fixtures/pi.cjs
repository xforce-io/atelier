#!/usr/bin/env node
// Deterministic Pi wire protocol fixture. No model or native CLI acceptance.
const fs = require('node:fs'), net = require('node:net'), crypto = require('node:crypto');
const args = process.argv.slice(2);
const arg = name => args[args.indexOf(name) + 1];
const emit = value => process.stdout.write(JSON.stringify(value) + '\n');
let input = '';
process.stdin.setEncoding('utf8');
process.stdin.on('data', chunk => input += chunk);
process.stdin.on('end', async () => {
  try {
    const prompt = JSON.parse(input);
    const request = JSON.parse(prompt.workMessage);
    const file = arg('--session');
    const session = fs.existsSync(file) ? JSON.parse(fs.readFileSync(file, 'utf8').split('\n')[0]) : { type: 'session', id: crypto.randomUUID(), cwd: process.cwd() };
    fs.appendFileSync(file, (fs.existsSync(file) ? '' : JSON.stringify(session) + '\n') + JSON.stringify({ prompt }) + '\n');
    fs.writeFileSync('last-prompt.json', JSON.stringify(prompt));
    fs.writeFileSync('last-args.json', JSON.stringify(args));
    emit(session);
    const results = [];
    for (const [i, call] of (request.calls || []).entries()) {
      results.push(await new Promise((resolve, reject) => {
        const socket = net.connect(process.env.MILKIE_TOOL_SOCKET);
        let buffer = '';
        socket.on('error', reject);
        socket.on('connect', () => socket.write(JSON.stringify({ id: String(i), nativeCallId: 'native-' + i, ...call }) + '\n'));
        socket.on('data', chunk => {
          buffer += chunk;
          if (buffer.includes('\n')) { socket.end(); resolve(JSON.parse(buffer.split('\n')[0])); }
        });
      }));
    }
    fs.writeFileSync('last-results.json', JSON.stringify(results));
    if (request.hold) { setInterval(() => {}, 1000); return; }
    emit({ type: 'message_end', message: { role: 'assistant', content: [{ type: 'text', text: 'fixture completed' }], stopReason: 'stop' } });
    emit({ type: 'agent_end', messages: [] });
  } catch { process.exitCode = 1; }
});
