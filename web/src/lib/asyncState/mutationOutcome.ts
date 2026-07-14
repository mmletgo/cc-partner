/**
 * Workbench mutation 传输中立 envelope：succeeded | unknown。
 *
 * Business Logic（为什么需要这个模块）:
 *   commit/push/merge/remove 在 timeout/network 下不能被猜成 notStarted/failed；
 *   Tauri 与 Mobile HTTP 共用成功通道 typed envelope，definitive 错误仍走 AppError。
 *
 * Code Logic（这个模块做什么）:
 *   定义 WorkbenchMutationEnvelope 与构造/判定 helper，不包含 ledger 或对账矩阵。
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
