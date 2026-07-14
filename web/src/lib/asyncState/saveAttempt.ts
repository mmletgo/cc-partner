/**
 * Safe-save 纯合同：捕获提交快照与版本，成功时保护保存期间的新编辑。
 *
 * Business Logic（为什么需要这个模块）:
 *   Settings/ClaudeMd 等保存路径若无条件回填提交时旧快照，会覆盖保存期间的新输入。
 *   需要与业务 API 解耦的 submittedSnapshot + editVersion + requestSeq 合同。
 *
 * Code Logic（这个模块做什么）:
 *   提供 createSaveAttempt / resolveSaveSuccess / resolveSaveFailure 纯函数；
 *   旧 seq 一律 applied:false；成功总是更新 baseline，仅当 draft 与版本未变才回填。
 */

/**
 * 一次保存提交的快照合同。
 *
 * Business Logic（为什么需要这个类型）:
 *   submit 时必须同时固定请求序号、提交快照与编辑版本，才能在响应返回时判定是否过期。
 *
 * Code Logic（字段说明）:
 *   requestSeq 单调递增；submittedSnapshot 为提交瞬间草稿副本；
 *   submittedEditVersion 为提交时的 editVersion。
 */
export interface SaveAttempt<T> {
  requestSeq: number;
  submittedSnapshot: T;
  submittedEditVersion: number;
}

/**
 * 保存响应解析结果。
 *
 * Business Logic（为什么需要这个类型）:
 *   调用方需要明确 baseline/draft/dirty/applied，避免静默覆盖或错误清脏。
 *
 * Code Logic（字段说明）:
 *   baseline 为已保存权威基线；draft 为应展示的编辑区内容；
 *   dirty 表示 draft 是否相对 baseline 仍脏；applied 表示本次响应是否被采纳。
 */
export interface SaveResolution<T> {
  baseline: T;
  draft: T;
  dirty: boolean;
  applied: boolean;
}

/**
 * resolveSaveSuccess 的输入。
 */
export interface ResolveSaveSuccessInput<T> {
  attempt: SaveAttempt<T>;
  currentRequestSeq: number;
  currentDraft: T;
  currentEditVersion: number;
  serverValue: T;
  /** 可选：当前 baseline；stale 时原样返回。缺省用 submittedSnapshot 作 stale baseline。 */
  currentBaseline?: T;
}

/**
 * resolveSaveFailure 的输入。
 */
export interface ResolveSaveFailureInput<T> {
  attempt: SaveAttempt<T>;
  currentRequestSeq: number;
  currentDraft: T;
  currentBaseline: T;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   submit 时需要原子捕获 attempt，避免手写字段遗漏。
 *
 * Code Logic（这个函数做什么）:
 *   返回包含 requestSeq、submittedSnapshot、submittedEditVersion 的 SaveAttempt。
 */
export function createSaveAttempt<T>(
  requestSeq: number,
  submittedSnapshot: T,
  submittedEditVersion: number,
): SaveAttempt<T> {
  return {
    requestSeq,
    submittedSnapshot,
    submittedEditVersion,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   判定两次快照是否仍表示“用户未在保存期间继续编辑”。
 *
 * Code Logic（这个函数做什么）:
 *   对原语用 ===；对象用 JSON 序列化严格相等（调用方应传可序列化快照）。
 */
function snapshotsEqual<T>(a: T, b: T): boolean {
  if (Object.is(a, b)) {
    return true;
  }
  if (typeof a !== 'object' || a === null || typeof b !== 'object' || b === null) {
    return false;
  }
  try {
    return JSON.stringify(a) === JSON.stringify(b);
  } catch {
    return false;
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   保存成功后必须更新已保存基线，但不得覆盖用户在保存期间输入的新草稿。
 *
 * Code Logic（这个函数做什么）:
 *   1) requestSeq 不匹配 → applied:false，保留当前 baseline/draft 与脏态。
 *   2) seq 匹配 → baseline 更新为 serverValue。
 *   3) 仅当 currentEditVersion === submittedEditVersion 且 draft 仍等于 submittedSnapshot
 *      时把 draft 回填为 serverValue 并 dirty=false；否则保留 currentDraft，dirty=true。
 */
export function resolveSaveSuccess<T>(
  input: ResolveSaveSuccessInput<T>,
): SaveResolution<T> {
  const {
    attempt,
    currentRequestSeq,
    currentDraft,
    currentEditVersion,
    serverValue,
    currentBaseline,
  } = input;

  if (attempt.requestSeq !== currentRequestSeq) {
    const baseline = currentBaseline !== undefined ? currentBaseline : attempt.submittedSnapshot;
    return {
      baseline,
      draft: currentDraft,
      dirty: !snapshotsEqual(currentDraft, baseline),
      applied: false,
    };
  }

  const unchanged =
    currentEditVersion === attempt.submittedEditVersion
    && snapshotsEqual(currentDraft, attempt.submittedSnapshot);

  if (unchanged) {
    return {
      baseline: serverValue,
      draft: serverValue,
      dirty: false,
      applied: true,
    };
  }

  return {
    baseline: serverValue,
    draft: currentDraft,
    dirty: true,
    applied: true,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   保存失败时不得清草稿；旧 seq 的失败也不得扰动当前态。
 *
 * Code Logic（这个函数做什么）:
 *   seq 匹配 → applied:true 但 baseline 不变、draft 保留、dirty 按比较结果；
 *   seq 不匹配 → applied:false。
 */
export function resolveSaveFailure<T>(
  input: ResolveSaveFailureInput<T>,
): SaveResolution<T> {
  const { attempt, currentRequestSeq, currentDraft, currentBaseline } = input;
  const dirty = !snapshotsEqual(currentDraft, currentBaseline);

  if (attempt.requestSeq !== currentRequestSeq) {
    return {
      baseline: currentBaseline,
      draft: currentDraft,
      dirty,
      applied: false,
    };
  }

  return {
    baseline: currentBaseline,
    draft: currentDraft,
    dirty,
    applied: true,
  };
}
