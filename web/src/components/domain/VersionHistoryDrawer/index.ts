/**
 * VersionHistoryDrawer 导出入口
 *
 * Business Logic（为什么需要这个入口）:
 *   页面按统一路径引用版本历史抽屉与复制文本 helper。
 *
 * Code Logic（这个入口做什么）:
 *   re-export 组件、类型与 resolveVersionCopyText。
 */

export {
  VersionHistoryDrawer,
  resolveVersionCopyText,
} from './VersionHistoryDrawer';
export type {
  VersionHistoryDrawerProps,
  VersionHistoryNamespace,
} from './VersionHistoryDrawer';
