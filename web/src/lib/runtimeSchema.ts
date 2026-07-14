/**
 * 轻量可组合运行时 DTO decoder（公共门面）。
 *
 * Business Logic（为什么需要这个模块）:
 *   TypeScript 泛型只提供编译期假设；IPC/HTTP 边界遇到损坏或混合版本 DTO 时，
 *   必须在写入页面状态前 fail closed，且错误日志不得泄露 payload 正文。
 *   既有消费者统一从本路径导入，避免实现拆分后的 import 破碎。
 *
 * Code Logic（这个模块做什么）:
 *   作为 thin barrel：完整 Decoder/ContractDecodeError/组合式原语实现位于
 *   `runtimeSchemaPrimitives.ts`；本文件仅再导出公共 API，行为与拆分前完全一致。
 *   允许未知额外字段以兼容前向版本，必填字段/枚举/有限数严格；
 *   actualKind 仅分类 null/array/primitive/object。
 */

export {
  MAX_ARRAY_LENGTH,
  MAX_ARRAY_DEPTH,
  ContractDecodeError,
  actualKindOf,
  defineDecoder,
  numberDecoder,
  stringDecoder,
  booleanDecoder,
  literalDecoder,
  enumDecoder,
  nullableDecoder,
  optionalDecoder,
  arrayDecoder,
  recordDecoder,
  objectDecoder,
  unionDecoder,
  decodeValue,
  type Decoder,
  type ObjectDecoderOptions,
  type UnionBranchDecoder,
} from './runtimeSchemaPrimitives';
