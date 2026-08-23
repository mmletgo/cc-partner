/**
 * Agent Hub 已提交上下文 + 脏稿守卫。
 *
 * Business Logic（为什么需要）:
 *   页面与 Workbench 项目 Agent 共用同一套「未保存提示词不得默默丢掉」合同；
 *   关闭控制台、切 tab / 设备 / 范围都必须走 Stay / Save / Discard。
 *   用户级三槽切 Agent：公共/适配/独有草稿保留；原始栏按原生文件绝对路径守卫。
 *   项目级原生文件按路径守卫：同一仓库 AGENTS.md 的 Agent 切换不弹窗。
 *
 * Code Logic（做什么）:
 *   以 controller.hubContext 为 requested；正文只消费 committed；dirty 时弹出同一 Dialog。
 *   confirmClose 在 busy 时拒绝；无脏稿直接通过；有脏稿等用户选择。
 */

import { useCallback, useEffect, useRef, useState, type ReactElement } from 'react';
import { Button, Dialog } from '@/components/primitives';
import {
  useInstructionThreePaneController,
  type UseInstructionThreePaneControllerResult,
} from './instructions';
import {
  useProjectInstructionFilesController,
  type UseProjectInstructionFilesControllerResult,
} from './projectInstructions';
import { peerAllowsUserInstructionThreePane } from './context/agentHubContext';
import type { UseAgentHubControllerResult } from './useAgentHubController';
import styles from './AgentHub.module.css';

export interface AgentHubSessionHandle {
  confirmClose: () => Promise<boolean>;
  isDirty: () => boolean;
}

export interface UseAgentHubSessionResult {
  committedHubContext: UseAgentHubControllerResult['hubContext'];
  onContextChange: (patch: Partial<UseAgentHubControllerResult['hubContext']>) => void;
  instructionThreePane: UseInstructionThreePaneControllerResult;
  projectInstructionFiles: UseProjectInstructionFilesControllerResult;
  contextSwitchDialog: ReactElement;
  confirmClose: () => Promise<boolean>;
  isDirty: () => boolean;
}

/**
 * Business Logic: 壳层与正文始终只看已提交上下文，避免脏稿被 URL 或关闭动作冲掉。
 * Code Logic: requested≠committed 且 dirty 时回写 URL 并弹出 Dialog；confirmClose 复用同一 Dialog。
 */
export function useAgentHubSession(
  controller: UseAgentHubControllerResult,
): UseAgentHubSessionResult {
  const {
    hubContext: requestedHubContext,
    onContextChange: navigateContext,
    t,
  } = controller;
  const [committedHubContext, setCommittedHubContext] = useState(requestedHubContext);
  const [pendingHubContext, setPendingHubContext] = useState<
    UseAgentHubControllerResult['hubContext'] | null
  >(null);
  const [closeRequested, setCloseRequested] = useState(false);
  const contextStayRef = useRef<HTMLButtonElement | null>(null);
  const closeResolverRef = useRef<((ok: boolean) => void) | null>(null);
  const selectedCommittedPeer =
    committedHubContext.deviceId === null
      ? null
      : controller.shellPeers.find((peer) => peer.deviceId === committedHubContext.deviceId) ??
        null;
  const instructionThreePane = useInstructionThreePaneController({
    context: committedHubContext,
    t,
    enabled:
      (committedHubContext.tab === 'instructions' || committedHubContext.adaptView) &&
      committedHubContext.scope === 'user' &&
      (committedHubContext.deviceId === null ||
        peerAllowsUserInstructionThreePane(selectedCommittedPeer)),
  });
  const projectInstructionFiles = useProjectInstructionFilesController({
    projectKey: committedHubContext.projectKey,
    agent: committedHubContext.agent,
    enabled: committedHubContext.scope === 'project' && committedHubContext.projectKey !== null,
    active: committedHubContext.tab === 'instructions',
    t,
  });

  const committedFingerprint = JSON.stringify(committedHubContext);
  const requestedFingerprint = JSON.stringify(requestedHubContext);
  const instructionsDirty = instructionThreePane.dirty || projectInstructionFiles.dirty;

  const isBusy =
    controller.actionBusy ||
    controller.portableActionBusy ||
    controller.portablePull.busy ||
    controller.userMirror.busy ||
    instructionThreePane.actionBusy ||
    projectInstructionFiles.actionBusy ||
    controller.lanPushOpen ||
    controller.portablePullOpen ||
    controller.userMirrorOpen ||
    controller.portableActionOpen;

  const shouldGuardNextContext = useCallback(
    (next: UseAgentHubControllerResult['hubContext']) => {
      if (projectInstructionFiles.shouldGuardContextChange(next)) return true;
      return instructionThreePane.shouldGuardContextChange(next);
    },
    [instructionThreePane, projectInstructionFiles],
  );

  useEffect(() => {
    if (requestedFingerprint === committedFingerprint) return;
    const timeoutId = window.setTimeout(() => {
      if (shouldGuardNextContext(requestedHubContext)) {
        navigateContext(committedHubContext);
        setPendingHubContext(requestedHubContext);
        return;
      }
      setCommittedHubContext(requestedHubContext);
    }, 0);
    return () => window.clearTimeout(timeoutId);
  }, [
    committedFingerprint,
    committedHubContext,
    navigateContext,
    requestedHubContext,
    requestedFingerprint,
    shouldGuardNextContext,
  ]);

  const onContextChange = useCallback(
    (patch: Partial<UseAgentHubControllerResult['hubContext']>) => {
      const next = {
        ...committedHubContext,
        ...patch,
      };
      if (next.scope === 'user') next.projectKey = null;
      else next.deviceId = null;
      if (JSON.stringify(next) === committedFingerprint) return;
      if (shouldGuardNextContext(next)) {
        setPendingHubContext(next);
        return;
      }
      setCommittedHubContext(next);
      navigateContext(next);
    },
    [
      committedFingerprint,
      committedHubContext,
      navigateContext,
      shouldGuardNextContext,
    ],
  );

  const finishClose = useCallback((ok: boolean) => {
    const resolve = closeResolverRef.current;
    closeResolverRef.current = null;
    setCloseRequested(false);
    setPendingHubContext(null);
    resolve?.(ok);
  }, []);

  const stayInCommittedContext = useCallback(() => {
    if (closeRequested) {
      finishClose(false);
      return;
    }
    setPendingHubContext(null);
    navigateContext(committedHubContext);
  }, [closeRequested, committedHubContext, finishClose, navigateContext]);

  const commitPendingContext = useCallback(() => {
    instructionThreePane.discardDraftForContextChange();
    projectInstructionFiles.discardDraftForContextChange();
    if (closeRequested) {
      finishClose(true);
      return;
    }
    if (!pendingHubContext) return;
    setCommittedHubContext(pendingHubContext);
    navigateContext(pendingHubContext);
    setPendingHubContext(null);
  }, [
    closeRequested,
    finishClose,
    instructionThreePane,
    navigateContext,
    pendingHubContext,
    projectInstructionFiles,
  ]);

  const saveAndCommitPendingContext = useCallback(async () => {
    const saved = projectInstructionFiles.dirty
      ? await projectInstructionFiles.saveAllDirty()
      : await instructionThreePane.saveBlocks();
    if (!saved) return;
    if (closeRequested) {
      finishClose(true);
      return;
    }
    if (!pendingHubContext) return;
    setCommittedHubContext(pendingHubContext);
    navigateContext(pendingHubContext);
    setPendingHubContext(null);
  }, [
    closeRequested,
    finishClose,
    instructionThreePane,
    navigateContext,
    pendingHubContext,
    projectInstructionFiles,
  ]);

  const confirmClose = useCallback((): Promise<boolean> => {
    if (isBusy) return Promise.resolve(false);
    if (!instructionsDirty) return Promise.resolve(true);
    return new Promise((resolve) => {
      closeResolverRef.current = resolve;
      setCloseRequested(true);
    });
  }, [instructionsDirty, isBusy]);

  const isDirty = useCallback(() => instructionsDirty, [instructionsDirty]);
  const saveDisabled = projectInstructionFiles.dirty
    ? projectInstructionFiles.actionBusy || !projectInstructionFiles.dirty
    : instructionThreePane.actionBusy ||
      !instructionThreePane.state.blocksDirty ||
      instructionThreePane.state.externalDrift;
  const saveLoading = projectInstructionFiles.dirty
    ? projectInstructionFiles.busyAction === 'save'
    : instructionThreePane.busyAction === 'save';
  const discardDisabled =
    instructionThreePane.actionBusy || projectInstructionFiles.actionBusy;
  const saveLabel = projectInstructionFiles.dirty
    ? controller.t('agentHub:instructions.projectFiles.contextSave')
    : controller.t('agentHub:instructions.threePane.contextSave');

  const dialogOpen = pendingHubContext !== null || closeRequested;
  const contextSwitchDialog = (
    <Dialog
      open={dialogOpen}
      titleId="agent-hub-context-change-title"
      onClose={stayInCommittedContext}
      initialFocusRef={contextStayRef}
    >
      <div className={styles.dialogBody} data-testid="agent-hub-context-change-dialog">
        <h2 id="agent-hub-context-change-title" className={styles.drawerTitle}>
          {controller.t('agentHub:instructions.threePane.contextSwitchTitle')}
        </h2>
        <p className={styles.drawerSubtitle}>
          {controller.t('agentHub:instructions.threePane.contextSwitchWarning')}
        </p>
        <div className={styles.dialogActions}>
          <Button
            ref={contextStayRef}
            variant="primary"
            size="sm"
            onClick={stayInCommittedContext}
            data-testid="agent-hub-context-stay"
          >
            {controller.t('agentHub:instructions.threePane.contextStay')}
          </Button>
          <Button
            variant="secondary"
            size="sm"
            disabled={saveDisabled}
            loading={saveLoading}
            onClick={() => void saveAndCommitPendingContext()}
            data-testid="agent-hub-context-save"
          >
            {saveLabel}
          </Button>
          <Button
            variant="danger"
            size="sm"
            disabled={discardDisabled}
            onClick={commitPendingContext}
            data-testid="agent-hub-context-discard"
          >
            {controller.t('agentHub:instructions.threePane.contextDiscard')}
          </Button>
        </div>
      </div>
    </Dialog>
  );

  return {
    committedHubContext,
    onContextChange,
    instructionThreePane,
    projectInstructionFiles,
    contextSwitchDialog,
    confirmClose,
    isDirty,
  };
}
