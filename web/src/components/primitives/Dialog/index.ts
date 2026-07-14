/**
 * Dialog 组件入口
 *
 * Business Logic（为什么需要这个入口）:
 *   统一 Dialog / useModalLayer 的导入路径，业务与上层 primitives 只需依赖目录 barrel。
 *
 * Code Logic（这个入口做什么）:
 *   重导出 Dialog、useModalLayer 与相关类型。
 */

export { Dialog } from './Dialog';
export type { DialogProps } from './Dialog';
export { useModalLayer, getFocusableElements } from './useModalLayer';
export type { ModalLayerOptions } from './useModalLayer';
