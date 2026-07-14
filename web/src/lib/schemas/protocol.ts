/**
 * P2P health / capabilities / error envelope 运行时 schema。
 *
 * Business Logic（为什么需要这个模块）:
 *   Mobile/Attention 能力探测与 P2P 错误分类依赖 health 与错误信封形状；
 *   损坏字段不得被当成合法能力或错误码。
 *
 * Code Logic（这个模块做什么）:
 *   解码 snake_case HealthResponse 与 P2pErrorEnvelope；
 *   protocol_version/capabilities 缺失时显式 legacy default（0 / []）。
 */

import {
  arrayDecoder,
  booleanDecoder,
  numberDecoder,
  objectDecoder,
  recordDecoder,
  stringDecoder,
  type Decoder,
} from '../runtimeSchema';

/**
 * P2P health 协议元数据（可被 attention gate 复用）。
 *
 * Business Logic（为什么需要这个类型）:
 *   旧后端可能缺 protocol_version/capabilities，gate 需安全回落。
 *
 * Code Logic（字段说明）:
 *   snake_case 对齐 /api/health；legacy 缺省 0/[]。
 */
export interface ProtocolHealthInfo {
  protocol_version: number;
  capabilities: string[];
}

/**
 * 完整 /api/health 响应。
 *
 * Business Logic（为什么需要这个类型）:
 *   Devices/Mobile 需要 ok/device/port/ts 与协议元数据。
 *
 * Code Logic（字段说明）:
 *   snake_case 对齐 Rust HealthResponse。
 */
export interface ProtocolHealthResponse extends ProtocolHealthInfo {
  ok: boolean;
  device_id: string;
  device_name: string;
  http_port: number;
  ts: number;
}

/**
 * P2P 标准错误信封。
 *
 * Business Logic（为什么需要这个类型）:
 *   边界错误分类依赖 code/request_id/retryable，不能只靠 error 文案。
 *
 * Code Logic（字段说明）:
 *   对齐 Rust P2pErrorEnvelope；details 为 string 键的未知 JSON 原语/对象占位。
 */
export interface ProtocolErrorEnvelope {
  error: string;
  code: string;
  request_id: string;
  retryable: boolean;
  details: Record<string, unknown>;
}

/** 宽松 unknown 值：仅用于 details 前向兼容，不做深层校验。 */
const unknownValueDecoder: Decoder<unknown> = {
  name: 'unknown',
  decode(value: unknown): unknown {
    return value;
  },
};

/**
 * Business Logic（为什么需要这个 decoder）:
 *   Attention/capability gate 只需协议元数据切片。
 *
 * Code Logic（这个 decoder 做什么）:
 *   解码 protocol_version/capabilities；缺失时显式 default 0/[]。
 */
export const protocolHealthInfoDecoder: Decoder<ProtocolHealthInfo> = objectDecoder<ProtocolHealthInfo>(
  'ProtocolHealthInfo',
  {
    protocol_version: numberDecoder,
    capabilities: arrayDecoder(stringDecoder),
  },
  {
    defaults: {
      protocol_version: 0,
      capabilities: [],
    },
  },
);

/**
 * Business Logic（为什么需要这个 decoder）:
 *   完整 health 响应用于设备信息与能力探测。
 *
 * Code Logic（这个 decoder 做什么）:
 *   必填 ok/device_id/device_name/http_port/ts；协议字段带 legacy default。
 */
export const protocolHealthResponseDecoder: Decoder<ProtocolHealthResponse> =
  objectDecoder<ProtocolHealthResponse>(
    'ProtocolHealthResponse',
    {
      ok: booleanDecoder,
      device_id: stringDecoder,
      device_name: stringDecoder,
      http_port: numberDecoder,
      ts: numberDecoder,
      protocol_version: numberDecoder,
      capabilities: arrayDecoder(stringDecoder),
    },
    {
      defaults: {
        protocol_version: 0,
        capabilities: [],
      },
    },
  );

/**
 * Business Logic（为什么需要这个 decoder）:
 *   HTTP 失败体需要稳定 code/request_id，禁止把任意 JSON 当信封。
 *
 * Code Logic（这个 decoder 做什么）:
 *   严格校验 error/code/request_id/retryable；details 缺省 {}。
 */
export const protocolErrorEnvelopeDecoder: Decoder<ProtocolErrorEnvelope> =
  objectDecoder<ProtocolErrorEnvelope>(
    'ProtocolErrorEnvelope',
    {
      error: stringDecoder,
      code: stringDecoder,
      request_id: stringDecoder,
      retryable: booleanDecoder,
      details: recordDecoder(unknownValueDecoder),
    },
    {
      defaults: {
        details: {},
      },
    },
  );
