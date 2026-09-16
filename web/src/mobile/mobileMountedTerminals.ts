/**
 * 移动端常驻 xterm 上限。超过后淘汰最久未用、且不是当前 active 的 session。
 */
export const MAX_MOUNTED_MOBILE_TERMINALS = 8;

export interface NextMountedMobileSessionIdsInput {
  previous: readonly string[];
  preferred: readonly string[];
  activeId: string | null;
  /** 任一项目缓存里仍存在的 session；不在此集合的 previous 会被丢掉。 */
  liveIds: readonly string[];
}

/**
 * Business Logic（为什么需要这个函数）:
 *   手机内存有限，不能像桌面那样无限挂 xterm；但切项目/切窗口时又要尽量复用已挂载实例。
 *
 * Code Logic（这个函数做什么）:
 *   顺序：active → preferred 其余 → previous 里仍要保留的 id。去重后截到 MAX。
 *   active 始终保留；截断时从尾部丢掉最老的非 active id。
 */
export function nextMountedMobileSessionIds(
  input: NextMountedMobileSessionIdsInput,
): string[] {
  const live = new Set(input.liveIds);
  if (input.activeId) live.add(input.activeId);
  const seen = new Set<string>();
  const ordered: string[] = [];
  const push = (id: string | null | undefined): void => {
    if (!id || seen.has(id) || !live.has(id)) return;
    seen.add(id);
    ordered.push(id);
  };
  push(input.activeId);
  for (const id of input.preferred) push(id);
  for (const id of input.previous) push(id);
  if (ordered.length <= MAX_MOUNTED_MOBILE_TERMINALS) return ordered;
  const kept: string[] = [];
  for (const id of ordered) {
    if (kept.length < MAX_MOUNTED_MOBILE_TERMINALS) {
      kept.push(id);
      continue;
    }
    break;
  }
  if (input.activeId && !kept.includes(input.activeId)) {
    kept[kept.length - 1] = input.activeId;
  }
  return kept;
}
