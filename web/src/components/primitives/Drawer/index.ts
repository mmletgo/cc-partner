/**
 * Drawer 组件入口
 *
 * Business Logic（为什么需要这个入口）:
 *   统一 Drawer 导入路径，业务侧栏与导航抽屉复用同一原语。
 *
 * Code Logic（这个入口做什么）:
 *   重导出 Drawer 组件与 DrawerProps 类型。
 */

export { Drawer } from './Drawer';
export type { DrawerProps } from './Drawer';
