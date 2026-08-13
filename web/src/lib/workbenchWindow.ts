/**
 * 桌面 Workbench 多窗 label / role / auto slot 纯函数合同。
 *
 * Business Logic（为什么需要这个模块）:
 *   后续卫星窗会各自持有独立 desktop:auto 现场；label 与 slot 必须前后端一致，
 *   避免主窗与 workbench-1..4 互相覆盖，或 overlay 窗误走 layout。
 *
 * Code Logic（这个模块做什么）:
 *   解析 main / workbench-1..4 / overlay 角色，并在合法工作台窗上派生 auto slot key。
 */

export const MAIN_WINDOW_LABEL = 'main';
export const WORKBENCH_WINDOW_LABEL_PREFIX = 'workbench-';
export const MAX_WORKBENCH_SATELLITE_WINDOWS = 4;

export type WorkbenchSatelliteSlot = 1 | 2 | 3 | 4;
export type WorkbenchWindowRole = 'main' | 'satellite' | 'overlay';

const SATELLITE_SLOTS = [1, 2, 3, 4] as const satisfies readonly WorkbenchSatelliteSlot[];

/**
 * Business Logic（为什么需要这个函数）:
 *   AppShell / restore / 关闭选择都依赖当前 OS 窗角色，不能靠页面路径猜测。
 *
 * Code Logic（这个函数做什么）:
 *   main → main；workbench-1..4 → satellite；其余（含 overlay 前缀与非法 satellite）→ overlay。
 */
export function parseWorkbenchWindowRole(label: string): WorkbenchWindowRole {
  if (label === MAIN_WINDOW_LABEL) return 'main';
  if (parseSatelliteSlot(label) !== null) return 'satellite';
  return 'overlay';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   建窗与 occupancy 必须用稳定 slot 编号，禁止 8-hex 或随意后缀。
 *
 * Code Logic（这个函数做什么）:
 *   把 1..4 拼成 `workbench-N`。
 */
export function satelliteWindowLabel(slot: WorkbenchSatelliteSlot): string {
  return `${WORKBENCH_WINDOW_LABEL_PREFIX}${slot}`;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   从窗口 label 取卫星序号；非法后缀不得当成可分配 slot。
 *
 * Code Logic（这个函数做什么）:
 *   仅接受精确 `workbench-1`..`workbench-4`，否则返回 null。
 */
export function parseSatelliteSlot(label: string): WorkbenchSatelliteSlot | null {
  if (!label.startsWith(WORKBENCH_WINDOW_LABEL_PREFIX)) return null;
  const suffix = label.slice(WORKBENCH_WINDOW_LABEL_PREFIX.length);
  for (const slot of SATELLITE_SLOTS) {
    if (suffix === String(slot)) return slot;
  }
  return null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   每窗独立 auto layout，主窗继续用历史 `desktop:auto`，卫星窗不得写同一行。
 *
 * Code Logic（这个函数做什么）:
 *   main → `desktop:auto`；workbench-N → `desktop:auto:window:workbench-N`；其余抛错。
 */
export function layoutSlotKeyForWindowLabel(label: string): string {
  if (label === MAIN_WINDOW_LABEL) return 'desktop:auto';
  const slot = parseSatelliteSlot(label);
  if (slot !== null) return `desktop:auto:window:${satelliteWindowLabel(slot)}`;
  throw new Error(`workbench_window_invalid_layout_label:${label}`);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   persist / delete 必须识别哪些 slot 属于窗口 auto 合同，不能放行 named 或越界窗。
 *
 * Code Logic（这个函数做什么）:
 *   仅 `desktop:auto` 与 `desktop:auto:window:workbench-[1-4]` 为真。
 */
export function isWindowAutoSlotKey(slotKey: string): boolean {
  if (slotKey === 'desktop:auto') return true;
  const prefix = 'desktop:auto:window:';
  if (!slotKey.startsWith(prefix)) return false;
  return parseSatelliteSlot(slotKey.slice(prefix.length)) !== null;
}
