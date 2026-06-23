/**
 * PromptOptimizer 页面入口
 *
 * Business Logic（为什么需要这个入口）:
 *   让路由和页面 barrel export 通过稳定路径导入 Prompt 优化页。
 *
 * Code Logic（这个入口做什么）:
 *   re-export 页面组件实现。
 */

export { PromptOptimizer } from './PromptOptimizer';
