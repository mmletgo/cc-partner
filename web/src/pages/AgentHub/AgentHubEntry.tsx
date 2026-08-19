/**
 * Agent Hub 路由入口：用户级工作台，或把旧项目深链转到 Workbench 项目 Agent。
 *
 * Business Logic（为什么需要）:
 *   生产路径的 Agent Hub 只查看用户级 × 设备。旧书签 `/agent-hub?scope=project&project=…`
 *   以及 `section=projectInstructions` 必须落到对应项目的 Workbench 项目 Agent，
 *   不得再把项目当作 Hub 的查看上下文，也不得静默掉回用户级 Hub。
 *
 * Code Logic（做什么）:
 *   parse 后若 scope=project：有 projectKey 则 replace-navigate 到 Workbench Project Agent URL；
 *   无 projectKey 则进入 Workbench 但不打开控制台。否则渲染用户级 AgentHub。
 */

import type { ReactElement } from 'react';
import { Navigate, useSearchParams } from 'react-router-dom';
import { AgentHub } from './AgentHub';
import { parseAgentHubContext } from './context/agentHubContext';
import { buildWorkbenchProjectAgentDeepLink } from '@/pages/Workbench/workbenchDeepLink';

/**
 * Business Logic: `/agent-hub` 路由的唯一入口，先拦截项目查看上下文再挂载页面。
 * Code Logic: 项目 scope → `/workbench?projectId=&view=projectAgent`；否则 <AgentHub />。
 */
export function AgentHubEntry(): ReactElement {
  const [searchParams] = useSearchParams();
  const ctx = parseAgentHubContext(searchParams);
  if (ctx.scope === 'project') {
    if (!ctx.projectKey) {
      return <Navigate to="/workbench" replace />;
    }
    return (
      <Navigate
        to={buildWorkbenchProjectAgentDeepLink({
          projectId: ctx.projectKey,
          agent: ctx.agent,
          tab: ctx.tab,
          instructionLane: ctx.instructionLane,
          assetLane: ctx.assetLane,
          adapt: ctx.adaptView,
        })}
        replace
      />
    );
  }
  return <AgentHub />;
}
