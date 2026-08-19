/**
 * Workbench 项目 Agent 控制台：冻结当前项目并复用 Agent Hub 视图。
 *
 * Business Logic（为什么需要）:
 *   项目级指令/资产必须挂在当前 Workbench 项目上，而不是 Hub 里的 user|project 切换器。
 *   控制台复用同一套 Hub 视图与脏稿守卫，关闭前要确认未保存草稿。
 *
 * Code Logic（做什么）:
 *   以 workbenchProject host 挂载 useAgentHubController；scopeLock=project；
 *   通过 imperative handle 把 confirmClose 交给 Workbench 标题栏 mutex。
 */

import { forwardRef, useImperativeHandle, type ForwardedRef, type ReactElement } from 'react';
import { AgentHubView } from './AgentHub';
import { useAgentHubController } from './useAgentHubController';
import { useAgentHubSession, type AgentHubSessionHandle } from './useAgentHubSession';

export type WorkbenchProjectAgentConsoleHandle = AgentHubSessionHandle;

export interface WorkbenchProjectAgentConsoleProps {
  projectKey: string;
  frozenProjectLabel: string;
  unsavedFilesNotice?: string | null;
}

/**
 * Business Logic: Workbench 只传入当前项目身份；控制台内部不得再切换项目或设备。
 * Code Logic: host 冻结 projectKey；session 提供脏稿 Dialog 与 confirmClose。
 */
export const WorkbenchProjectAgentConsole = forwardRef(function WorkbenchProjectAgentConsole(
  props: WorkbenchProjectAgentConsoleProps,
  ref: ForwardedRef<WorkbenchProjectAgentConsoleHandle>,
): ReactElement {
  const { projectKey, frozenProjectLabel, unsavedFilesNotice } = props;
  const controller = useAgentHubController({
    kind: 'workbenchProject',
    projectKey,
  });
  const session = useAgentHubSession(controller);

  useImperativeHandle(
    ref,
    () => ({
      confirmClose: session.confirmClose,
      isDirty: session.isDirty,
    }),
    [session.confirmClose, session.isDirty],
  );

  return (
    <>
      <AgentHubView
        {...controller}
        hubContext={session.committedHubContext}
        onContextChange={session.onContextChange}
        instructionThreePane={session.instructionThreePane}
        embedded
        scopeLock="project"
        frozenProjectLabel={frozenProjectLabel}
        unsavedFilesNotice={unsavedFilesNotice}
      />
      {session.contextSwitchDialog}
    </>
  );
});
