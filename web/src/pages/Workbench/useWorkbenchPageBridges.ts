/**
 * useWorkbenchPageBridges — Workbench 页面喂给各域 controller 的稳定回调工厂。
 *
 * Business Logic（为什么需要）:
 *   错误/消息文案翻译、Prompt 优化配置加载与流式写入都是只依赖 t / 设备名 / 静态 API 的纯胶水，
 *   无页面 state。集中到此 hook 让 Workbench.tsx 满足 ≤1200 行预算；每个回调经 useCallback 稳定，
 *   避免下游 controller 的 useCallback 依赖每次渲染抖动（会反触发 loadSessions / loadDir 等效应，
 *   把 terminal-status 事件更新覆盖回 running）。
 *
 * Code Logic（做什么）:
 *   返回 8 个 translate* + loadPromptOptimizerConfig + streamPromptToTerminal，均 useCallback。
 *   不是域 controller（不持有 state/effect），故不进入 controllers/index.ts 七 controller 合同。
 */
import { useCallback } from 'react';
import type { TFunction } from 'i18next';
import { configApi } from '@/api/config';
import { promptOptimizerApi } from '@/api/promptOptimizer';
import type { PromptOptimizerFillLanguage } from '@/lib/types';
import type { PromptOptimizerConfigLoadResult } from './controllers/useWorkbenchPromptOptimizerController';
import type { WorkbenchTerminalErrorKey } from './controllers/useWorkbenchTerminalController';
import type { WorkbenchWorktreeGitErrorKey } from './controllers/useWorkbenchWorktreeGitController';
import type { WorkbenchFileErrorKey, WorkbenchFileMessageKey } from './controllers/useWorkbenchFileController';

export function useWorkbenchPageBridges(t: TFunction<'workbench'>, deviceName: string | undefined) {
  const translateTerminalError = useCallback(
    (key: WorkbenchTerminalErrorKey): string => t(`errors.${key}`),
    [t],
  );
  const translateWorktreeError = useCallback(
    (key: WorkbenchWorktreeGitErrorKey): string => t(`errors.${key}`),
    [t],
  );
  const translateWorktreeMessage = useCallback(
    (
      key: 'mergeConfirm' | 'removeConfirm' | 'checkSourceMessage',
      vars?: Record<string, unknown>,
    ): string => {
      if (key === 'mergeConfirm') return t('worktrees.mergeConfirm', vars);
      if (key === 'removeConfirm') return t('worktrees.removeConfirm', vars);
      return t('mergeStages.messages.checkSource');
    },
    [t],
  );
  const translateFileError = useCallback(
    (key: WorkbenchFileErrorKey): string => t(`errors.${key}`),
    [t],
  );
  const translateFileMessage = useCallback(
    (key: WorkbenchFileMessageKey, vars?: Record<string, unknown>): string => {
      if (key === 'saved') return t('fileWorkspace.saved');
      if (key === 'formatted') return t('fileWorkspace.formatted');
      if (key === 'pathCopied') return t('pathCopied');
      if (key === 'confirmCloseDirtyFile') return t('confirmCloseDirtyFile', vars);
      if (key === 'confirmDeleteDirtyFiles') return t('confirmDeleteDirtyFiles', vars);
      return t('confirmDeletePath', vars);
    },
    [t],
  );
  const translatePromptFillFailed = useCallback(
    (): string => t('promptOptimizer.fillFailed'),
    [t],
  );
  const translatePromptOptimizeFailed = useCallback(
    (): string => t('promptOptimizer.optimizeFailed'),
    [t],
  );
  const translatePromptRemoteOffline = useCallback(
    (): string =>
      t('remoteOfflineNotice', {
        device: deviceName ?? t('emptyValue'),
      }),
    [deviceName, t],
  );
  const loadPromptOptimizerConfig = useCallback(
    async (): Promise<PromptOptimizerConfigLoadResult> => {
      const config = await configApi.get();
      return {
        promptOptimizerHotkey: config.promptOptimizerHotkey,
        promptOptimizerFillLanguage: config.promptOptimizerFillLanguage,
      };
    },
    [],
  );
  const streamPromptToTerminal = useCallback(
    (
      prompt: string,
      options: {
        workingDirectory?: string | null;
        targetLanguage: PromptOptimizerFillLanguage;
        sessionId: string;
      },
    ) => promptOptimizerApi.streamToTerminal(prompt, options),
    [],
  );
  return {
    translateTerminalError,
    translateWorktreeError,
    translateWorktreeMessage,
    translateFileError,
    translateFileMessage,
    translatePromptFillFailed,
    translatePromptOptimizeFailed,
    translatePromptRemoteOffline,
    loadPromptOptimizerConfig,
    streamPromptToTerminal,
  };
}
