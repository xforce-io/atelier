import { Ajv, type ValidateFunction } from 'ajv';
import type { HostToolSchema, HostToolSpec, ToolSchema } from '@freemanxu/milkie';

type Schema = Record<string, unknown>;
function object(value: unknown): Schema {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('cli_tool_schema_invalid');
  return value as Schema;
}

/** milkie requires a type at every property node. JSON Schema permits enum /
 * const without one. Add the implied primitive type, retaining ALL constraints
 * for the CLI and the complete validator at the host callback boundary. */
function typedSchema(value: unknown, depth = 0): Schema {
  if (depth > 8) throw new Error('cli_tool_schema_too_deep');
  const schema = structuredClone(object(value));
  if (schema.type === undefined) {
    const values = schema.const !== undefined ? [schema.const] : schema.enum;
    if (!Array.isArray(values) || values.length === 0) throw new Error('cli_tool_schema_type_required');
    const kinds = new Set(values.map(item => typeof item));
    const kind = [...kinds][0];
    if (kinds.size !== 1 || !['string', 'number', 'boolean'].includes(kind!)) throw new Error('cli_tool_schema_type_required');
    schema.type = kind;
  }
  if (!['object', 'array', 'string', 'number', 'integer', 'boolean'].includes(schema.type as string)) throw new Error('cli_tool_schema_type_unsupported');
  if (schema.properties !== undefined) {
    schema.properties = Object.fromEntries(Object.entries(object(schema.properties)).map(([name, child]) => [name, typedSchema(child, depth + 1)]));
  }
  if (schema.type === 'array') schema.items = typedSchema(schema.items, depth + 1);
  // Branches remain full schemas. Their properties also need explicit types
  // for CLI consumers; the original branch semantics must not be flattened.
  for (const keyword of ['oneOf', 'anyOf', 'allOf']) {
    if (schema[keyword] !== undefined) {
      if (!Array.isArray(schema[keyword])) throw new Error('cli_tool_schema_invalid');
      schema[keyword] = schema[keyword].map(branch => typedSchema(branch, depth + 1));
    }
  }
  return schema;
}

export class CliTools {
  readonly specs: HostToolSpec[];
  private readonly validators = new Map<string, ValidateFunction>();
  constructor(tools: ToolSchema[]) {
    if (tools.length === 0 || tools.length > 32) throw new Error('cli_tool_count_invalid');
    // No coercion, removal of unknown fields, defaults, or asynchronous schema
    // loading. Invalid input is rejected; neither schema nor arguments change.
    const ajv = new Ajv({ strict: true, allErrors: false });
    this.specs = tools.map(tool => {
      if (!/^[a-z][a-z0-9_]{0,40}$/.test(tool.name) || this.validators.has(tool.name)
        || !tool.description.trim() || tool.description.length > 4000) throw new Error('cli_tool_definition_invalid');
      const schema = typedSchema(tool.inputSchema);
      if (schema.type !== 'object' || schema.additionalProperties !== false) throw new Error('cli_tool_schema_invalid');
      this.validators.set(tool.name, ajv.compile(schema));
      return { name: tool.name, description: tool.description, inputSchema: schema as unknown as HostToolSchema };
    });
  }
  validate(name: string, input: unknown): 'allowed' | 'invalid_input' | 'rejected' {
    const validate = this.validators.get(name);
    if (!validate) return 'rejected';
    return validate(input) ? 'allowed' : 'invalid_input';
  }
}
