/**
 * 轻量可组合运行时 DTO decoder。
 *
 * Business Logic（为什么需要这个模块）:
 *   TypeScript 泛型只提供编译期假设；IPC/HTTP 边界遇到损坏或混合版本 DTO 时，
 *   必须在写入页面状态前 fail closed，且错误日志不得泄露 payload 正文。
 *
 * Code Logic（这个模块做什么）:
 *   提供 Decoder 接口、ContractDecodeError 与无外部依赖的组合式原语；
 *   允许未知额外字段以兼容前向版本，必填字段/枚举/有限数严格；
 *   actualKind 仅分类 null/array/primitive/object。
 */

/** 数组最大长度，防止恶意/损坏 payload 膨胀内存。 */
export const MAX_ARRAY_LENGTH = 10_000;

/** 嵌套数组最大深度，防止递归爆炸。 */
export const MAX_ARRAY_DEPTH = 32;

/**
 * 运行时解码器契约。
 *
 * Business Logic（为什么需要这个接口）:
 *   各域 schema 与 invoke/HTTP 边界需要统一的 decode 形状，便于组合与错误定位。
 *
 * Code Logic（这个接口做什么）:
 *   name 用于错误 contract 标识；decode 在 path 处把 unknown 收敛为 T 或抛错。
 */
export interface Decoder<T> {
  readonly name: string;
  decode(value: unknown, path?: string): T;
}

/**
 * 契约解码失败错误。
 *
 * Business Logic（为什么需要这个类）:
 *   调用方可区分契约失败与业务/网络错误；日志只能写 contract/path/kind，禁止序列化 payload。
 *
 * Code Logic（这个类做什么）:
 *   扩展 Error，附带 contract/path/actualKind；message 仅含安全元数据。
 */
export class ContractDecodeError extends Error {
  readonly contract: string;
  readonly path: string;
  readonly actualKind: string;

  /**
   * Business Logic（为什么需要这个构造函数）:
   *   统一构造不含 payload 的契约错误。
   *
   * Code Logic（这个函数做什么）:
   *   设置 name/message 与只读元数据字段。
   */
  constructor(contract: string, path: string, actualKind: string) {
    super(`Contract "${contract}" failed at ${path}: got ${actualKind}`);
    this.name = 'ContractDecodeError';
    this.contract = contract;
    this.path = path;
    this.actualKind = actualKind;
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   错误诊断只需粗粒度 kind，禁止反射出具体字符串/数字内容。
 *
 * Code Logic（这个函数做什么）:
 *   返回 null | array | object | primitive 之一。
 */
export function actualKindOf(value: unknown): 'null' | 'array' | 'object' | 'primitive' {
  if (value === null) return 'null';
  if (Array.isArray(value)) return 'array';
  if (typeof value === 'object') return 'object';
  return 'primitive';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   所有原语失败路径需要统一抛出 ContractDecodeError。
 *
 * Code Logic（这个函数做什么）:
 *   用 contract/path 与 value 的 kind 构造并抛出。
 */
function fail(contract: string, path: string, value: unknown): never {
  throw new ContractDecodeError(contract, path, actualKindOf(value));
}

/**
 * Business Logic（为什么需要这个函数）:
 *   对象字段 path 需要稳定 JSON-path 风格路径，便于测试与日志。
 *
 * Code Logic（这个函数做什么）:
 *   拼接 `base.key`。
 */
function fieldPath(base: string, key: string): string {
  return `${base}.${key}`;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   数组元素 path 需要 `items[i]` 形式定位 malformed 字段。
 *
 * Code Logic（这个函数做什么）:
 *   返回 `base[index]`。
 */
function indexPath(base: string, index: number): string {
  return `${base}[${index}]`;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   各域 schema 需要给 decoder 命名以便 ContractDecodeError.contract 可读。
 *
 * Code Logic（这个函数做什么）:
 *   包装已有 decode 函数为带 name 的 Decoder。
 */
export function defineDecoder<T>(name: string, decode: (value: unknown, path: string) => T): Decoder<T> {
  return {
    name,
    decode(value: unknown, path = '$'): T {
      return decode(value, path);
    },
  };
}

/** 有限 number decoder。 */
export const numberDecoder: Decoder<number> = defineDecoder('number', (value, path) => {
  if (typeof value !== 'number' || !Number.isFinite(value)) fail('number', path, value);
  return value;
});

/** 字符串 decoder。 */
export const stringDecoder: Decoder<string> = defineDecoder('string', (value, path) => {
  if (typeof value !== 'string') fail('string', path, value);
  return value;
});

/** 布尔 decoder。 */
export const booleanDecoder: Decoder<boolean> = defineDecoder('boolean', (value, path) => {
  if (typeof value !== 'boolean') fail('boolean', path, value);
  return value;
});

/**
 * Business Logic（为什么需要这个函数）:
 *   枚举字面量字段（status/origin 等）必须严格匹配已知集合。
 *
 * Code Logic（这个函数做什么）:
 *   值必须等于给定字面量，否则 fail。
 */
export function literalDecoder<T extends string | number | boolean>(expected: T): Decoder<T> {
  const name = `literal(${String(expected)})`;
  return defineDecoder(name, (value, path) => {
    if (value !== expected) fail(name, path, value);
    return expected;
  });
}

/**
 * Business Logic（为什么需要这个函数）:
 *   多值枚举（direction/status）需要闭集校验。
 *
 * Code Logic（这个函数做什么）:
 *   值须为 values 之一的 string；不匹配则 fail。
 */
export function enumDecoder<T extends string>(name: string, values: readonly T[]): Decoder<T> {
  const set = new Set<string>(values);
  return defineDecoder(name, (value, path) => {
    if (typeof value !== 'string' || !set.has(value)) fail(name, path, value);
    return value as T;
  });
}

/**
 * Business Logic（为什么需要这个函数）:
 *   后端可空字段（null）与缺失 optional 字段语义不同，需要显式组合。
 *
 * Code Logic（这个函数做什么）:
 *   null 原样返回；否则委托 inner。
 */
export function nullableDecoder<T>(inner: Decoder<T>): Decoder<T | null> {
  const name = `nullable(${inner.name})`;
  return defineDecoder(name, (value, path) => {
    if (value === null) return null;
    return inner.decode(value, path);
  });
}

/**
 * Business Logic（为什么需要这个函数）:
 *   可选字段缺失时返回 undefined，存在时严格解码。
 *
 * Code Logic（这个函数做什么）:
 *   undefined 返回 undefined；否则委托 inner（不把 null 当缺失）。
 */
export function optionalDecoder<T>(inner: Decoder<T>): Decoder<T | undefined> {
  const name = `optional(${inner.name})`;
  return defineDecoder(name, (value, path) => {
    if (value === undefined) return undefined;
    return inner.decode(value, path);
  });
}

/**
 * Business Logic（为什么需要这个函数）:
 *   列表 DTO 需要元素级校验并限制长度/深度。
 *
 * Code Logic（这个函数做什么）:
 *   校验 array，强制 maxLength/depth，逐元素 decode。
 */
export function arrayDecoder<T>(
  inner: Decoder<T>,
  options: { maxLength?: number; maxDepth?: number; depth?: number } = {},
): Decoder<T[]> {
  const maxLength = options.maxLength ?? MAX_ARRAY_LENGTH;
  const maxDepth = options.maxDepth ?? MAX_ARRAY_DEPTH;
  const depth = options.depth ?? 0;
  const name = `array(${inner.name})`;
  return defineDecoder(name, (value, path) => {
    if (!Array.isArray(value)) fail(name, path, value);
    if (depth > maxDepth) fail(name, path, value);
    if (value.length > maxLength) fail(name, path, value);
    const out: T[] = [];
    for (let i = 0; i < value.length; i += 1) {
      out.push(inner.decode(value[i], indexPath(path, i)));
    }
    return out;
  });
}

/**
 * Business Logic（为什么需要这个函数）:
 *   字符串键映射（vectorClock 等）需要值类型校验。
 *
 * Code Logic（这个函数做什么）:
 *   要求 plain object，逐 value decode；允许任意 string key。
 */
export function recordDecoder<T>(inner: Decoder<T>): Decoder<Record<string, T>> {
  const name = `record(${inner.name})`;
  return defineDecoder(name, (value, path) => {
    if (value === null || typeof value !== 'object' || Array.isArray(value)) fail(name, path, value);
    const out: Record<string, T> = {};
    for (const [key, entry] of Object.entries(value as Record<string, unknown>)) {
      out[key] = inner.decode(entry, fieldPath(path, key));
    }
    return out;
  });
}

/**
 * objectDecoder 选项。
 *
 * Business Logic（为什么需要这个类型）:
 *   legacy 字段默认值必须按 schema 显式声明。
 *
 * Code Logic（字段说明）:
 *   defaults 仅在字段缺失时注入；调用方用显式泛型 T 固定 shape，避免 defaults 收窄推断。
 */
export interface ObjectDecoderOptions<T> {
  defaults?: { [K in keyof T]?: T[K] };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   结构化 DTO 是主契约形态；必填字段严格，未知额外字段前向兼容保留忽略。
 *
 * Code Logic（这个函数做什么）:
 *   校验 plain object；缺失字段先查 defaults；其余委托 shape decoder。
 *   显式泛型 T 时，defaults 不参与 shape 推断，避免 `{ details: {} }` 收窄整个对象。
 */
export function objectDecoder<T extends object>(
  name: string,
  shape: { [K in keyof T]-?: Decoder<T[K]> },
  options: ObjectDecoderOptions<T> = {},
): Decoder<T> {
  const keys = Object.keys(shape) as (keyof T & string)[];
  return defineDecoder(name, (value, path) => {
    if (value === null || typeof value !== 'object' || Array.isArray(value)) fail(name, path, value);
    const source = value as Record<string, unknown>;
    const out = {} as T;
    for (const key of keys) {
      const missing = !(key in source);
      let fieldValue: unknown = source[key];
      if (missing && options.defaults && Object.prototype.hasOwnProperty.call(options.defaults, key)) {
        fieldValue = options.defaults[key] as unknown;
      }
      out[key] = shape[key].decode(fieldValue, fieldPath(path, key));
    }
    return out;
  });
}

/**
 * 可参与 union 的 decoder 分支（返回 unknown，再收敛为 T）。
 *
 * Business Logic（为什么需要这个类型）:
 *   判别联合的各分支具体类型不同，不能强制同一 Decoder 泛型参数。
 *
 * Code Logic（字段说明）:
 *   与 Decoder 同形，但 decode 返回 unknown，便于分支异构。
 */
export interface UnionBranchDecoder {
  readonly name: string;
  decode(value: unknown, path?: string): unknown;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   判别联合（task view / attention target）需要依次尝试分支。
 *
 * Code Logic（这个函数做什么）:
 *   按序 decode；全部失败时以最后分支 kind 报错（不拼接 payload）；
 *   成功结果断言为 T（调用方负责 union 类型正确性）。
 */
export function unionDecoder<T>(name: string, branches: readonly UnionBranchDecoder[]): Decoder<T> {
  return defineDecoder(name, (value, path) => {
    let lastKind = actualKindOf(value);
    for (const branch of branches) {
      try {
        return branch.decode(value, path) as T;
      } catch (reason) {
        if (reason instanceof ContractDecodeError) {
          lastKind = reason.actualKind as typeof lastKind;
          continue;
        }
        throw reason;
      }
    }
    throw new ContractDecodeError(name, path, lastKind);
  });
}

/**
 * Business Logic（为什么需要这个函数）:
 *   调用方需要统一入口执行 decode。
 *
 * Code Logic（这个函数做什么）:
 *   委托 decoder.decode。
 */
export function decodeValue<T>(decoder: Decoder<T>, value: unknown, path = '$'): T {
  return decoder.decode(value, path);
}
