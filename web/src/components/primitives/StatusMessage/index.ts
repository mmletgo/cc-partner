/**
 * StatusMessage 组件入口
 *
 * Business Logic（为什么需要这个入口）:
 *   统一 StatusMessage 对外导入路径，供 pages/domain 与 barrel 复用。
 *
 * Code Logic（这个入口做什么）:
 *   重导出 StatusMessage 组件与相关类型。
 */

export { StatusMessage } from './StatusMessage';
export type {
  StatusMessageProps,
  StatusMessageTone,
  StatusMessageLive,
} from './StatusMessage';
