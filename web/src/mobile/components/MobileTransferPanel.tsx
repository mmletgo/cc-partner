/**
 * 移动端文件传输面板壳。
 *
 * Business Logic（为什么需要这个组件）:
 *   `/mobile` 需要独立传输入口；壳层只组合 controller 与纯视图，避免 view 碰 API。
 *
 * Code Logic（这个组件做什么）:
 *   调用 useMobileTransferController 并把 view model 交给 MobileTransferView。
 */

import type { ReactElement } from 'react';
import { useMobileTransferController } from '../controllers/useMobileTransferController';
import { MobileTransferView } from './MobileTransferView';

/**
 * MobileTransferPanel（移动端文件传输面板）
 *
 * Business Logic（为什么需要这个组件）:
 *   Workbench 懒加载边界需要 named export；HTTP 调用只属于 controller。
 *
 * Code Logic（这个组件做什么）:
 *   hook → view，无额外 JSX 业务分支。
 */
export function MobileTransferPanel(): ReactElement {
  const viewModel = useMobileTransferController();
  return <MobileTransferView {...viewModel} />;
}
