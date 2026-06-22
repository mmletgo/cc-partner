/**
 * Tauri IPC 客户端封装 - 基于 invoke 的统一调用入口
 *
 * Business Logic（为什么需要这个模块）:
 *   迁移到 Tauri 后，前端不再有任何本地 HTTP 调用，统一通过 invoke() 直达 Rust 后端。
 *   本模块对 invoke 做薄封装：把 Rust 后端 reject 的错误（{ error: "中文消息" }）
 *   规整为带 message 的 Error，供各 service 与组件层用 instanceof Error / err.message 统一处理。
 *
 * Code Logic（这个模块做什么）:
 *   - `invoke<T>(cmd, args?)`：透传 @tauri-apps/api/core 的 invoke，泛型保留返回类型。
 *   - `normalizeError`：invoke reject 的值可能是 { error } 对象或字符串，统一转成 Error。
 *   - 旧的 ApiError / status / buildUrl / api.get/post/put/del 全部删除（无 HTTP status 概念）。
 */

import { invoke as tauriInvoke } from '@tauri-apps/api/core';

/**
 * 将 invoke reject 抛出的任意值规整为 Error。
 *
 * Rust 后端的 AppError 经 serde 序列化为 `{ error: "中文消息" }`，
 * Tauri 会把它作为 reject reason 透传给前端。这里提取出可读消息，
 * 既兼容 { error } 对象，也兼容裸字符串或其他形态。
 */
function normalizeError(reason: unknown): Error {
  if (reason instanceof Error) return reason;
  if (typeof reason === 'string') return new Error(reason);
  if (reason && typeof reason === 'object') {
    const obj = reason as Record<string, unknown>;
    const msg = obj.error ?? obj.message;
    if (typeof msg === 'string') return new Error(msg);
  }
  return new Error(String(reason));
}

/**
 * 调用 Rust 后端命令。任意 reject 都会被规整为 Error 抛出。
 * @param cmd - 命令名（对应 Rust #[tauri::command] 函数名）
 * @param args - 命令参数（字段名需与 Rust 函数签名一致，camelCase）
 */
export async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await tauriInvoke<T>(cmd, args);
  } catch (reason) {
    throw normalizeError(reason);
  }
}

/** 统一 IPC 调用入口（语义与 invoke 一致，保留 api.* 风格便于 service 引用） */
export const api = {
  invoke,
};
