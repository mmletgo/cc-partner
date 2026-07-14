/**
 * RouteErrorBoundary barrel
 *
 * Business Logic（为什么需要这个入口）:
 *   layout 层与 App 路由需要稳定导入路径，避免散落实现文件路径。
 *
 * Code Logic（这个文件做什么）:
 *   re-export 组件与 props 类型。
 */

export { RouteErrorBoundary } from './RouteErrorBoundary';
export type { RouteErrorBoundaryProps } from './RouteErrorBoundary';
