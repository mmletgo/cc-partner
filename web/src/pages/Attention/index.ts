/**
 * Attention 页面 barrel。
 *
 * Business Logic（为什么需要这个入口）:
 *   路由与测试需要稳定的 Attention / AttentionView 导入路径。
 *
 * Code Logic（这个入口做什么）:
 *   re-export 页面组件与可测试视图。
 */
export { Attention, AttentionView } from './Attention';
export type { AttentionViewProps } from './Attention';
