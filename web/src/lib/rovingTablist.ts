/**
 * Roving tablist 纯索引辅助。
 *
 * Business Logic（为什么需要这个模块）:
 *   Workbench 终端 tabs、inspector tabs、文件 tabs 等横向 tablist 需要统一的
 *   Arrow/Home/End 焦点索引语义，避免各处手写 wrap/边界逻辑不一致。
 *
 * Code Logic（这个模块做什么）:
 *   导出 RovingTabKey 与 getRovingTabIndex：根据当前索引、按键与总数计算下一索引；
 *   ArrowLeft/Right 循环，Home/End 跳到首/尾；非法输入返回 currentIndex。
 */

/**
 * Roving tablist 支持的方向键集合。
 *
 * Business Logic（为什么需要这个类型）:
 *   共享 contract 只接受左右方向与首尾跳转，避免各组件自行扩展不一致的按键集合。
 *
 * Code Logic（字段说明）:
 *   字符串字面量联合：ArrowLeft / ArrowRight / Home / End。
 */
export type RovingTabKey = 'ArrowLeft' | 'ArrowRight' | 'Home' | 'End';

/**
 * Business Logic（为什么需要这个函数）:
 *   键盘用户在 tablist 内用方向键/Home/End 切换时，需要确定性的下一索引；
 *   多处 tablist 共用同一 wrap 语义，避免左边界/右边界处理不一致。
 *
 * Code Logic（这个函数做什么）:
 *   在 count<=0 或 currentIndex 越界时返回 max(0, currentIndex) 的安全值；
 *   ArrowRight/ArrowLeft 对 count 取模循环；Home→0，End→count-1；未知 key 原样返回 currentIndex。
 */
export function getRovingTabIndex(
  currentIndex: number,
  key: RovingTabKey,
  count: number,
): number {
  if (count <= 0) {
    return 0;
  }

  const safeIndex =
    Number.isFinite(currentIndex) && currentIndex >= 0 && currentIndex < count
      ? Math.trunc(currentIndex)
      : 0;

  switch (key) {
    case 'ArrowRight':
      return (safeIndex + 1) % count;
    case 'ArrowLeft':
      return (safeIndex - 1 + count) % count;
    case 'Home':
      return 0;
    case 'End':
      return count - 1;
    default:
      return safeIndex;
  }
}
