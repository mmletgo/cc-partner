/**
 * WorkbenchFleetPage — `/workbench/fleet` 路由页
 *
 * Business Logic（为什么需要这个页面）:
 *   Fleet 作为 Workbench 二级入口，不应塞进 1200 行 Workbench.tsx。
 *
 * Code Logic（这个组件做什么）:
 *   调用 useLanAgentFleet + WorkbenchFleetView。
 */

import type { ReactElement } from 'react';
import { useLanAgentFleet } from '@/hooks/useLanAgentFleet';
import { WorkbenchFleetView } from './views/WorkbenchFleetView';

/**
 * Business Logic（为什么需要这个组件）:
 *   用户从 Rail 进入 Fleet 详情。
 *
 * Code Logic（这个组件做什么）:
 *   挂载 hook 并渲染只读视图。
 */
export function WorkbenchFleetPage(): ReactElement {
  const { snapshot, loading, error, refresh } = useLanAgentFleet({ enabled: true });
  return (
    <WorkbenchFleetView
      snapshot={snapshot}
      loading={loading}
      error={error}
      onRefresh={() => void refresh()}
    />
  );
}
