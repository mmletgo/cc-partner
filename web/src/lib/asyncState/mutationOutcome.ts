/**
 * Workbench mutation 传输中立 envelope：succeeded | unknown。
 *
 * Business Logic（为什么需要这个模块）:
 *   commit/push/merge/remove 在 timeout/network 下不能被猜成 notStarted/failed；
 *   Tauri 与 Mobile HTTP 共用成功通道 typed envelope，definitive 错误仍走 AppError。
 *   unknown 恢复必须靠 typed code / envelope kind，禁止解析本地化文案。
 *
 * Code Logic（这个模块做什么）:
 *   定义 WorkbenchMutationEnvelope 与构造/判定 helper，以及 typed unknown Error。
 */

/**
 * 不确定传输类别（浏览器无法可靠区分 connect/first-byte/not-started）。
 */
export type MutationTransportClass = 'timeout' | 'network';

/**
 * Workbench mutation 结果 envelope（成功通道）。
 *
 * Business Logic（为什么需要这个类型）:
 *   unknown 只携带 caller 已知的 operation id 与 transport class，
 *   不得伪造未收到的 reconciliation intent。
 *
 * Code Logic（联合形态）:
 *   succeeded 带权威 value；unknown 仅带 clientOperationId 与可选 transportClass。
 */
export type WorkbenchMutationEnvelope<T> =
  | { kind: 'succeeded'; value: T; clientOperationId: string }
  | {
      kind: 'unknown';
      clientOperationId: string;
      transportClass?: MutationTransportClass;
    };

/** 稳定错误 code：unknown 恢复路径用它判定，禁止依赖本地化 message。 */
export const MUTATION_UNKNOWN_ERROR_CODE = 'mutationUnknown' as const;

/**
 * typed unknown mutation 错误。
 *
 * Business Logic（为什么需要这个类型）:
 *   父级 merge/remove 路径需要把 unknown 抛给面板；面板必须在 EN/zh 下都能识别，
 *   不能靠 `message.includes('结果未知')`。
 *
 * Code Logic（字段说明）:
 *   code 固定为 mutationUnknown；clientOperationId 供 same-id 对账/重试。
 */
export class WorkbenchMutationUnknownError extends Error {
  readonly code = MUTATION_UNKNOWN_ERROR_CODE;
  readonly clientOperationId: string;

  /**
   * Business Logic（为什么需要这个函数）:
   *   构造可跨层传播的 typed unknown，携带稳定 operation id。
   *
   * Code Logic（这个函数做什么）:
   *   super(message)；name 固定；保存 clientOperationId。
   */
  constructor(clientOperationId: string, message?: string) {
    super(message ?? MUTATION_UNKNOWN_ERROR_CODE);
    this.name = 'WorkbenchMutationUnknownError';
    this.clientOperationId = clientOperationId;
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   后端/transport 确认成功后构造 succeeded envelope。
 *
 * Code Logic（这个函数做什么）:
 *   返回 { kind:'succeeded', value, clientOperationId }。
 */
export function mutationSucceeded<T>(
  value: T,
  clientOperationId: string,
): WorkbenchMutationEnvelope<T> {
  return { kind: 'succeeded', value, clientOperationId };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   timeout/network 等不确定路径必须走 unknown，禁止静默成功或盲重放。
 *
 * Code Logic（这个函数做什么）:
 *   返回 { kind:'unknown', clientOperationId, transportClass? }。
 */
export function mutationUnknown(
  clientOperationId: string,
  transportClass?: MutationTransportClass,
): WorkbenchMutationEnvelope<never> {
  return transportClass === undefined
    ? { kind: 'unknown', clientOperationId }
    : { kind: 'unknown', clientOperationId, transportClass };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   控制器需要窄化 envelope 以分支 succeeded / reconciling。
 *
 * Code Logic（这个函数做什么）:
 *   kind === 'succeeded' 时为 true。
 */
export function isMutationSucceeded<T>(
  envelope: WorkbenchMutationEnvelope<T>,
): envelope is Extract<WorkbenchMutationEnvelope<T>, { kind: 'succeeded' }> {
  return envelope.kind === 'succeeded';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   控制器在 unknown 时进入对账，不得把 value 当权威结果。
 *
 * Code Logic（这个函数做什么）:
 *   kind === 'unknown' 时为 true。
 */
export function isMutationUnknown<T>(
  envelope: WorkbenchMutationEnvelope<T>,
): envelope is Extract<WorkbenchMutationEnvelope<T>, { kind: 'unknown' }> {
  return envelope.kind === 'unknown';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   catch 路径必须 typed 识别 unknown，禁止中文/英文文案子串分支。
 *
 * Code Logic（这个函数做什么）:
 *   识别 WorkbenchMutationUnknownError 实例，或带 code===mutationUnknown 的 Error-like 对象。
 */
export function isWorkbenchMutationUnknownError(
  reason: unknown,
): reason is WorkbenchMutationUnknownError {
  if (reason instanceof WorkbenchMutationUnknownError) return true;
  if (typeof reason !== 'object' || reason === null) return false;
  const code = (reason as { code?: unknown }).code;
  return code === MUTATION_UNKNOWN_ERROR_CODE;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   从 typed unknown 错误取出 clientOperationId，供 same-id 对账。
 *
 * Code Logic（这个函数做什么）:
 *   优先读 WorkbenchMutationUnknownError.clientOperationId；否则读可选字段；都没有返回 null。
 */
export function getUnknownMutationClientOperationId(reason: unknown): string | null {
  if (reason instanceof WorkbenchMutationUnknownError) {
    return reason.clientOperationId;
  }
  if (typeof reason !== 'object' || reason === null) return null;
  const id = (reason as { clientOperationId?: unknown }).clientOperationId;
  return typeof id === 'string' && id.length > 0 ? id : null;
}
