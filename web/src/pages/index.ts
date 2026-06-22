/**
 * 页面统一入口（barrel export）
 *
 * Business Logic（为什么需要这个入口）:
 *   - 业务模块按需 import 页面时，可走 `import { Home, Welcome } from '@/pages'` 单路径
 *   - 避免 router / AppShell / 测试用例对 pages/* 子路径细节的硬编码
 *
 * Code Logic（这个入口做什么）:
 *   统一 re-export 当前已实现的 Home、Welcome 页面。后续新增的 Transfer / Prompts / Devices / Settings 页面
 *   也按相同约定在此补齐即可。
 */

export { Home } from './Home';
export type { } from './Home';

export { Transfer } from './Transfer';
export type { } from './Transfer';

export { Devices } from './Devices';
export type { } from './Devices';

export { Settings } from './Settings';
export type { } from './Settings';

export { Welcome } from './Welcome';
export type { } from './Welcome';

export { DesignSystem } from './DesignSystem';

export { Scratchpad } from './Scratchpad';
export type { } from './Scratchpad';

// 区域截图选区页（独立于 AppShell，由 Tauri 选区窗口加载 /screenshot-overlay）
export { Overlay } from './Screenshot/Overlay';
