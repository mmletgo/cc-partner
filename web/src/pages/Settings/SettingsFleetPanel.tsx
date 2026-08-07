/**
 * SettingsFleetPanel — Settings「Fleet」tab 的只读 LAN Agent 聚合视图
 *
 * Business Logic（为什么需要这个组件）:
 *   Fleet 从工作台二级入口迁到 Settings，作为运维/诊断总览；不参与配置 save/defaults。
 *
 * Code Logic（这个组件做什么）:
 *   挂载 useLanAgentFleet 并渲染 WorkbenchFleetView（无「回工作台」主路径 chrome）。
 */

import type { ReactElement } from 'react';
import { useLanAgentFleet } from '@/hooks/useLanAgentFleet';
import { WorkbenchFleetView } from '@/pages/Workbench/views/WorkbenchFleetView';

/**
 * Business Logic（为什么需要这个组件）:
 *   Settings tab 需要懒加载 Fleet 快照，且不污染 settingsResources 11 端点合同。
 *
 * Code Logic（这个组件做什么）:
 *   enabled 固定 true（仅 active tab 挂载本 panel）；透传 snapshot/loading/error/refresh。
 */
export function SettingsFleetPanel(): ReactElement {
  const { snapshot, loading, error, refresh } = useLanAgentFleet({ enabled: true });
  return (
    <WorkbenchFleetView
      snapshot={snapshot}
      loading={loading}
      error={error}
      onRefresh={() => void refresh()}
      showWorkbenchLink={false}
    />
  );
}

SettingsFleetPanel.displayName = 'SettingsFleetPanel';
