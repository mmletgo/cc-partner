/**
 * Orchestrator runtime 传输层结构化错误。
 *
 * Business Logic（为什么需要这个模块）:
 *   桌面 hook / 移动 store 在 invoke/HTTP 抛错时，不能再靠 Error.message 中英文关键词猜测 offline；
 *   需要 adapter 层给出可判别的 transport kind，四态权威仍以成功 DTO 的 remoteStatus 为准。
 *
 * Code Logic（这个模块做什么）:
 *   定义 `OrchestratorRuntimeTransportError`（kind: network | protocol | unknown），
 *   并提供 kind 判定与从 unknown 原因构造错误的 helper。
 */

/**
 * 传输错误分类。
 *
 * Business Logic（为什么需要这个类型）:
 *   仅 network 才可在存在 remote live 缓存时展示 offline warm cache；protocol/unknown 不得关键词推断 offline。
 *
 * Code Logic（取值说明）:
 *   network=传输/连通失败；protocol=HTTP 非 2xx 或契约层失败；unknown=无法结构化分类。
 */
export type OrchestratorRuntimeTransportKind = 'network' | 'protocol' | 'unknown';

/**
 * OrchestratorRuntimeTransportError（runtime 传输错误）。
 *
 * Business Logic（为什么需要这个类）:
 *   hook/store 需要 instanceof + kind 分支，而不是解析本地化 message。
 *
 * Code Logic（这个类做什么）:
 *   继承 Error，附带稳定 kind；message 仅供展示。
 */
export class OrchestratorRuntimeTransportError extends Error {
  readonly kind: OrchestratorRuntimeTransportKind;

  /**
   * Business Logic（为什么需要这个构造）:
   *   adapter 在 Tauri reject / fetch 失败时需要带 kind 抛出。
   *
   * Code Logic（这个函数做什么）:
   *   设置 message、kind 与 name，便于调试与 instanceof 检测。
   */
  constructor(message: string, kind: OrchestratorRuntimeTransportKind) {
    super(message);
    this.name = 'OrchestratorRuntimeTransportError';
    this.kind = kind;
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   catch 分支需要判断错误是否为显式 network transport，才能决定是否展示 offline 缓存。
 *
 * Code Logic（这个函数做什么）:
 *   仅当 err 是 OrchestratorRuntimeTransportError 且 kind==='network' 时返回 true。
 */
export function isOrchestratorRuntimeNetworkTransportError(error: unknown): boolean {
  return (
    error instanceof OrchestratorRuntimeTransportError && error.kind === 'network'
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   adapter 需要把任意 reject 原因规整为带 kind 的 Error，且不依赖中文/英文关键词猜 offline。
 *
 * Code Logic（这个函数做什么）:
 *   已是本类则原样返回；否则用 message 包装为 kind=unknown（默认不提升为 network）。
 */
export function toOrchestratorRuntimeTransportError(
  reason: unknown,
  kind: OrchestratorRuntimeTransportKind = 'unknown',
): OrchestratorRuntimeTransportError {
  if (reason instanceof OrchestratorRuntimeTransportError) {
    return reason;
  }
  if (reason instanceof Error) {
    return new OrchestratorRuntimeTransportError(reason.message || String(reason), kind);
  }
  if (typeof reason === 'string') {
    return new OrchestratorRuntimeTransportError(reason, kind);
  }
  if (reason && typeof reason === 'object') {
    const obj = reason as Record<string, unknown>;
    const msg = obj.error ?? obj.message;
    if (typeof msg === 'string' && msg.trim()) {
      return new OrchestratorRuntimeTransportError(msg, kind);
    }
  }
  return new OrchestratorRuntimeTransportError(String(reason), kind);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   面板/store 的 catch 路径必须保留 adapter 已抛出的 transport kind，
 *   否则 network reject 会被抹成普通 Error，warm offline 缓存永远进不去。
 *
 * Code Logic（这个函数做什么）:
 *   已是 Error（含 OrchestratorRuntimeTransportError）则原样返回；
 *   非 Error reject 才用 helper 包装为 kind=unknown 的传输错误。
 */
export function toRuntimeLoadError(reason: unknown): Error {
  if (reason instanceof Error) {
    return reason;
  }
  return toOrchestratorRuntimeTransportError(reason, 'unknown');
}
