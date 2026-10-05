import assert from 'node:assert/strict';
import { test } from 'node:test';
import { CliTools } from '../src/cli-tools.js';

const text = { type: 'string', minLength: 1, maxLength: 8 };
const schema = {
  type: 'object',
  properties: { kind: { enum: ['operation', 'blocked'] }, reason: text, reference: text },
  required: ['kind'], additionalProperties: false,
  oneOf: [
    { type: 'object', properties: { kind: { const: 'operation' }, reference: text }, required: ['kind', 'reference'], additionalProperties: false },
    { type: 'object', properties: { kind: { const: 'blocked' }, reason: text }, required: ['kind', 'reason'], additionalProperties: false },
  ],
};
const tool = { name: 'message_resolve', description: '结束当前投递的处理', inputSchema: schema };

test('CLI schema adds enum/const types without removing the core constraints', () => {
  const registry = new CliTools([tool]);
  const transmitted = registry.specs[0]!.inputSchema as unknown as typeof schema;
  assert.deepEqual(transmitted.properties.kind, { type: 'string', enum: ['operation', 'blocked'] });
  assert.deepEqual(transmitted.oneOf[0]!.properties.kind, { type: 'string', const: 'operation' });
  assert.equal(transmitted.properties.reason.maxLength, 8);
  assert.deepEqual(schema.properties.kind, { enum: ['operation', 'blocked'] });
  assert.equal(registry.validate(tool.name, { kind: 'operation', reference: 'op-1' }), 'allowed');
  assert.equal(registry.validate(tool.name, { kind: 'blocked', reason: '等待' }), 'allowed');
  for (const input of [
    { kind: 'invented', reason: '等待' }, { kind: 'operation' },
    { kind: 'operation', reason: '等待' }, { kind: 'blocked', reference: 'op-1' },
    { kind: 'blocked', reason: '' }, { kind: 'blocked', reason: 'x'.repeat(9) },
    { kind: 'blocked', reason: '等待', actor: 'someone-else' },
    { kind: 'blocked', reason: '等待', reference: 'op-1' },
  ]) assert.equal(registry.validate(tool.name, input), 'invalid_input');
  assert.equal(registry.validate('run_command', {}), 'rejected');
});

test('CLI validation does not coerce inputs or weaken array/numeric limits', () => {
  const registry = new CliTools([{ name: 'decision_request', description: '请求决定', inputSchema: {
    type: 'object', properties: { revision: { type: 'integer', minimum: 1 }, options: { type: 'array', maxItems: 2, items: text } },
    required: ['revision', 'options'], additionalProperties: false,
  } }]);
  assert.equal(registry.validate('decision_request', { revision: 1, options: ['yes'] }), 'allowed');
  const input = { revision: '1', options: ['yes'] };
  assert.equal(registry.validate('decision_request', input), 'invalid_input');
  assert.equal(input.revision, '1');
  for (const value of [{ revision: 0, options: [] }, { revision: 1, options: ['a', 'b', 'c'] }, { revision: 1.5, options: [] }]) {
    assert.equal(registry.validate('decision_request', value), 'invalid_input');
  }
});

test('unsupported or ambiguous schema fails preparation instead of dropping constraints', () => {
  for (const leaf of [{ enum: ['x', 1] }, { type: 'string', misspelledConstraint: true }, { $ref: 'https://example.invalid/schema' }]) {
    assert.throws(() => new CliTools([{ ...tool, inputSchema: { type: 'object', properties: { value: leaf }, additionalProperties: false } }]));
  }
  assert.throws(() => new CliTools([tool, tool]), /definition_invalid/);
});

test('reserved read tool has one CLI alias without granting its native name or allowing collisions', () => {
  const read = {...tool, name: 'read_file'};
  const registry = new CliTools([read]);
  assert.equal(registry.specs[0]!.name, 'atelier_read_file');
  assert.equal(registry.validate('read_file', {kind:'blocked',reason:'x'}), 'rejected');
  assert.equal(registry.validate('atelier_read_file', {kind:'blocked',reason:'x'}), 'allowed');
  assert.throws(() => new CliTools([read, {...read, name:'atelier_read_file'}]), /definition_invalid/);
  assert.throws(() => new CliTools([{...read, name:'atelier_read_file'}, read]), /definition_invalid/);
});
