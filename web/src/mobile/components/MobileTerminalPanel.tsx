import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { nextMountedMobileSessionIds } from '../mobileMountedTerminals';
import { MobileTerminalXtermSlot } from './MobileTerminalXtermSlot';
import type { ChangeEvent, CSSProperties, ReactElement, ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { FitAddon } from '@xterm/addon-fit';
import { Terminal } from '@xterm/xterm';
import '@xterm/xterm/css/xterm.css';
import {
  createHttpOrchestratorClientRequestId,
  httpWorkbenchTransport,
  workbenchHttp,
} from '@/api/workbenchHttp';
import { StatusMessage } from '@/components/primitives';
import {
  useWorkbenchTerminalBufferStore,
  useWorkbenchTerminalBuffers,
} from '@/hooks/workbenchTerminalBuffersContext';
import {
  getUnknownMutationClientOperationId,
  isWorkbenchMutationUnknownError,
} from '@/lib/asyncState/mutationOutcome';
import {
  ArrowRightIcon,
  CommitIcon,
  EditIcon,
  ImageIcon,
  MaximizeIcon,
  MinimizeIcon,
  MoreIcon,
  PlusIcon,
  PromptsIcon,
  SyncIcon,
  XIcon,
} from '@/lib/icons';
import type { Prompt, WorkbenchProject, WorkbenchSession, WorkbenchWorktree } from '@/lib/types';
import { workbenchTerminalOptions } from '@/pages/Workbench/terminalOptions';
import { fileToPngDataUrl } from '@/pages/Workbench/terminalImagePaste';
import {
  isMobileGitActionResponseCurrent,
  isMobileGitMergeResponseCurrent,
  isMobileMutationActionLocked,
  pickMobileMutationOperationId,
  type MobileGitActionContext,
  type MobileMutationPhase,
} from '../mobilePanelState';
import { executeMobileGitCommit } from '../mobileGitCommit';
import { useAutoDismissedStatus } from '../mobileTransientStatus';
import {
  canRunMobilePaneMutation,
  canShowMobileTerminalMergeFab,
  canSwitchMobilePane,
  computeMobileTerminalFabArc,
  computeMobileTerminalFabArcPath,
  emptyMobileSessionRuntimeState,
  getMobileCreatePaneDirection,
  getMobileTerminalChromeVisibility,
  mobileAgentForSession,
  selectPreferredMobileSession,
  type MobileSessionRuntimeState,
} from '../mobileWorkbenchState';
import styles from '../MobileWorkbench.module.css';
import {
  MobileTerminalInputStream,
  type MobileTerminalInputStreamState,
} from '../mobileTerminalInputStream';
import {
  findMobileTerminalHelperTextarea,
  leaveMobileTerminalTypingMode,
  MOBILE_TERMINAL_STICKY_TIMEOUT_MS,
  resolveMobileTerminalExtraKeyPress,
  toggleStickyModifier,
  type MobileTerminalExtraKeyDef,
  type MobileTerminalStickyModifier,
} from '../mobileTerminalExtraKeys';
import { writeClipboardText } from '../mobileClipboard';
import { MobileTerminalExtraKeys } from './MobileTerminalExtraKeys';
import { MobileFavoriteQuickInput } from './MobileFavoriteQuickInput';
import { MobilePromptOptimizerSheet } from './MobilePromptOptimizerSheet';
import { MobileHookRepairCard } from './MobileHookRepairCard';
import type { MobileHookRepair } from '../mobileHookRepair';
import { MobileWorktreeTabs, type MobileWorktreeTabsProps } from './MobileWorktreeTabs';
import { PointerPrimaryButton } from './PointerPrimaryButton';

const MIN_TERMINAL_COLS = 20;
const MIN_TERMINAL_ROWS = 6;
const DEFAULT_TERMINAL_SIZE = { cols: 80, rows: 24 };

const EMPTY_BACKGROUND_SESSIONS: WorkbenchSession[] = [];

export interface MobileTerminalPanelProps {
  project: WorkbenchProject | null;
  worktree: WorkbenchWorktree | null;
  /** 窗口 tab 上方的 worktree 条；全屏时由 chrome 可见性隐藏。 */
  worktreeBar?: MobileWorktreeTabsProps;
  sessions: WorkbenchSession[];
  /** 其它已访问项目的 session，仅用于跨项目常驻 xterm。 */
  backgroundSessions?: WorkbenchSession[];
  activeSession: WorkbenchSession | null;
  busy: boolean;
  /** session 运行时投影（terminal status + Agent）；点击 Agent 只选中 terminal。 */
  sessionRuntime?: MobileSessionRuntimeState;
  onSessionsChange: (next: WorkbenchSession[]) => void;
  onActiveSessionChange: (session: WorkbenchSession | null) => void;
  onRefreshSessions?: () => Promise<void> | void;
  /** Commit 成功后把权威 worktree 写回父级。 */
  onWorktreeChange?: (worktree: WorkbenchWorktree) => void;
  /** 终端合并 FAB 复用父级与 Git 面板相同的 dirty guard / envelope merge。 */
  onMergeWorktree?: (worktree: WorkbenchWorktree) => Promise<boolean>;
  /** unknown 对账确认成功后刷新 worktree 列表。 */
  onRefreshWorktrees?: (options?: {
    skipFileContextConfirm?: boolean;
    expectedProjectId?: string;
  }) => Promise<void> | void;
  /** 钩子 AI 修复启动后刷新并聚焦新终端。 */
  onFocusRepairSession?: (sessionId: string) => Promise<void> | void;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   后端 PTY resize 接受 u16 尺寸，移动端 xterm 在极小视口下也不能把 0 或异常值传给后端。
 *
 * Code Logic（这个函数做什么）:
 *   将输入数字四舍五入后限制在 min..65535；非法数字返回 min。
 */
function clampU16(value: number, min: number): number {
  if (!Number.isFinite(value)) return min;
  const rounded = Math.round(value);
  return Math.min(65535, Math.max(min, rounded));
}

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端新建 terminal window 时也应尽量使用当前可见终端区域尺寸，避免 TUI 首屏按默认列宽绘制后错位。
 *
 * Code Logic（这个函数做什么）:
 *   在屏幕外创建临时 xterm + FitAddon，按容器尺寸测出 cols/rows 后立即销毁。
 */
function measureMobileTerminalSize(container: HTMLElement | null): typeof DEFAULT_TERMINAL_SIZE {
  if (!container || container.clientWidth <= 0 || container.clientHeight <= 0) {
    return DEFAULT_TERMINAL_SIZE;
  }

  const host = document.createElement('div');
  const viewport = document.createElement('div');
  host.className = styles.mobileTerminalHost;
  viewport.className = styles.mobileTerminalViewport;
  host.style.position = 'fixed';
  host.style.left = '-10000px';
  host.style.top = '-10000px';
  host.style.width = `${container.clientWidth}px`;
  host.style.height = `${container.clientHeight}px`;
  host.style.visibility = 'hidden';
  host.style.pointerEvents = 'none';
  host.appendChild(viewport);
  document.body.appendChild(host);

  const terminal = new Terminal(workbenchTerminalOptions());
  const fit = new FitAddon();
  try {
    terminal.loadAddon(fit);
    terminal.open(viewport);
    fit.fit();
    return {
      cols: clampU16(terminal.cols, MIN_TERMINAL_COLS),
      rows: clampU16(terminal.rows, MIN_TERMINAL_ROWS),
    };
  } catch {
    return DEFAULT_TERMINAL_SIZE;
  } finally {
    terminal.dispose();
    host.remove();
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端终端操作失败时需要展示可读错误，并兼容非 Error 抛出值。
 *
 * Code Logic（这个函数做什么）:
 *   优先返回 Error.message；未知值转成字符串，空值回退到 fallback。
 */
function getErrorMessage(reason: unknown, fallback: string): string {
  if (reason instanceof Error && reason.message.trim()) return reason.message;
  const message = String(reason);
  return message && message !== 'undefined' && message !== 'null' ? message : fallback;
}

/**
 * MobileTerminalPanel（移动端真实终端面板）
 *
 * Business Logic（为什么需要这个组件）:
 *   `/mobile` 需要在手机浏览器中展示真实 Workbench tmux-backed terminal window/pane，并复用桌面端 session/window/pane 模型。
 *
 * Code Logic（这个组件做什么）:
 *   按 active project/worktree/session 渲染 session tabs、xterm viewport 和 window/pane 控制按钮；
 *   右下角默认一个折叠 FAB（有项目即可，不要求已开终端窗口；位置抬到常见 Agent 底部输入框之上），点开后沿左上象限环形展开；圆钮在弧上、文字标签沿同角度外侧对齐。
 *   动作为图片粘贴、Merge（仅非主工作区/非主分支或主工作区 canCollectMerge）、Git Commit（与桌面 Git 历史同口径，message=null）、
 *   Prompt 优化与收藏 Prompt；无 session 时贴图/优化/收藏禁用。再点触发钮、点遮罩或选完动作后收起；
 *   首屏通过 HTTP replay 写入历史 buffer，后续只消费外部 terminal buffer store 增量，输入/resize/focus/split/close 全部调用 HTTP transport。
 */
export function MobileTerminalPanel({
  project,
  worktree,
  worktreeBar,
  sessions,
  backgroundSessions,
  activeSession,
  busy,
  sessionRuntime = emptyMobileSessionRuntimeState(),
  onSessionsChange,
  onActiveSessionChange,
  onRefreshSessions,
  onWorktreeChange,
  onMergeWorktree,
  onRefreshWorktrees,
  onFocusRepairSession,
}: MobileTerminalPanelProps): ReactElement {
  const { t } = useTranslation(['workbench']);
  const { resetBuffer, removeBuffer } = useWorkbenchTerminalBuffers();
  const store = useWorkbenchTerminalBufferStore();
  const [actionBusy, setActionBusy] = useState<string | null>(null);
  const [panelError, setPanelError] = useState<string | null>(null);
  const [commitSuccess, setCommitSuccess] = useState<string | null>(null);
  const [terminalFullscreen, setTerminalFullscreen] = useState<boolean>(false);
  const [inputStreamState, setInputStreamState] = useState<MobileTerminalInputStreamState>({
    status: 'connecting',
  });
  const [stickyModifier, setStickyModifier] = useState<MobileTerminalStickyModifier | null>(null);
  const [favoriteSheetOpen, setFavoriteSheetOpen] = useState<boolean>(false);
  const [promptOptimizerSheetOpen, setPromptOptimizerSheetOpen] = useState<boolean>(false);
  const [commitPhase, setCommitPhase] = useState<MobileMutationPhase>('idle');
  const [mergePhase, setMergePhase] = useState<MobileMutationPhase>('idle');
  const [hookRepair, setHookRepair] = useState<MobileHookRepair | null>(null);
  const [selecting, setSelecting] = useState(false);
  const [selectedLineCount, setSelectedLineCount] = useState(1);
  const [selectionEmpty, setSelectionEmpty] = useState(true);
  const [selectionCopied, setSelectionCopied] = useState<string | null>(null);
  const [pasteImageBusy, setPasteImageBusy] = useState(false);
  const [fabMenuOpen, setFabMenuOpen] = useState(false);
  const commitOperationIdRef = useRef<string | null>(null);
  const mergeOperationIdRef = useRef<string | null>(null);
  const commitContextRef = useRef<MobileGitActionContext | null>(null);
  const surfaceRef = useRef<HTMLDivElement | null>(null);
  const viewportRef = useRef<HTMLDivElement | null>(null);
  const terminalRef = useRef<Terminal | null>(null);
  const boundSessionIdRef = useRef<string | null>(null);
  const inputEnabledRef = useRef<boolean>(false);
  const stickyModifierRef = useRef<MobileTerminalStickyModifier | null>(null);
  const stickyTimeoutRef = useRef<number | null>(null);
  const lastFocusedSessionIdRef = useRef<string | null>(null);
  const selectingRef = useRef(false);
  const pasteImageInputRef = useRef<HTMLInputElement | null>(null);
  const pasteImageBusyRef = useRef(false);
  const inputStreamRef = useRef<MobileTerminalInputStream | null>(null);
  useAutoDismissedStatus(commitSuccess, setCommitSuccess);
  useAutoDismissedStatus(selectionCopied, setSelectionCopied);

  const scopedSessions = useMemo(
    () =>
      project
        ? sessions.filter(
            (session) =>
              session.projectId === project.id &&
              (!worktree || session.worktreeId === worktree.id),
          )
        : [],
    [project, sessions, worktree],
  );
  const visibleSession = useMemo(
    () =>
      activeSession && scopedSessions.some((session) => session.id === activeSession.id)
        ? activeSession
        : null,
    [activeSession, scopedSessions],
  );
  const sessionId = visibleSession?.id ?? null;
  const allKnownSessions = useMemo(() => {
    const byId = new Map<string, WorkbenchSession>();
    for (const item of [...sessions, ...(backgroundSessions ?? EMPTY_BACKGROUND_SESSIONS)]) {
      byId.set(item.id, item);
    }
    return [...byId.values()];
  }, [backgroundSessions, sessions]);
  const [mountedIds, setMountedIds] = useState<string[]>([]);
  /* eslint-disable react-hooks/set-state-in-effect -- LRU 常驻列表必须随 session 集合收敛 */
  useEffect(() => {
    setMountedIds((previous) => {
      const next = nextMountedMobileSessionIds({
        previous,
        preferred: sessions.map((item) => item.id),
        activeId: sessionId,
        liveIds: allKnownSessions.map((item) => item.id),
      });
      if (
        previous.length === next.length &&
        previous.every((id, index) => id === next[index])
      ) {
        return previous;
      }
      return next;
    });
  }, [allKnownSessions, sessionId, sessions]);
  /* eslint-enable react-hooks/set-state-in-effect */
  const mountedSessions = useMemo(
    () =>
      mountedIds
        .map((id) => allKnownSessions.find((item) => item.id === id) ?? null)
        .filter((item): item is WorkbenchSession => item !== null),
    [allKnownSessions, mountedIds],
  );
  const visibleAgent = sessionId ? mobileAgentForSession(sessionRuntime, sessionId) : null;
  const activeAgentIdentity =
    sessionId && visibleAgent?.isActive ? `${sessionId}:${visibleAgent.id}` : null;
  const isActionDisabled = busy || actionBusy !== null;
  const canUsePaneActions = canRunMobilePaneMutation(visibleSession, isActionDisabled);
  const canSwitchPane = canSwitchMobilePane(visibleSession, isActionDisabled);
  // 收藏 Prompt 快捷输入按钮需要写终端输入流：session running 且 input stream ready 才启用。
  const canOpenFavoriteQuickInput =
    canUsePaneActions && inputStreamState.status === 'ready';
  const canPasteImage =
    Boolean(sessionId) &&
    visibleSession?.status === 'running' &&
    !busy &&
    inputStreamState.status === 'ready';
  const canCommitWorktree =
    Boolean(project && worktree) &&
    !busy &&
    (actionBusy === null || actionBusy === 'commit') &&
    (!isMobileMutationActionLocked(commitPhase) || commitPhase === 'unknown') &&
    !isMobileMutationActionLocked(mergePhase);
  const showMergeFab = Boolean(onMergeWorktree) && canShowMobileTerminalMergeFab(worktree);
  const mergeBusy = actionBusy === 'merge';
  const canMergeWorktree =
    showMergeFab &&
    Boolean(project && worktree) &&
    !busy &&
    (actionBusy === null || actionBusy === 'merge') &&
    !isMobileMutationActionLocked(commitPhase) &&
    (!isMobileMutationActionLocked(mergePhase) || mergePhase === 'unknown');
  const commitBusy = actionBusy === 'commit';
  const commitLabel = commitBusy
    ? t('workbench:mobile.gitPanel.committing')
    : t('workbench:worktrees.commit');
  const mergeLabel = mergeBusy
    ? t('workbench:mobile.gitPanel.merging')
    : t('workbench:worktrees.merge');
  const worktreeDirty = worktree != null && !worktree.status.clean;
  const isTerminalFullscreen = terminalFullscreen && visibleSession !== null;
  const terminalChrome = getMobileTerminalChromeVisibility(isTerminalFullscreen);
  const TerminalFullscreenIcon = terminalChrome.exitFullscreen ? MinimizeIcon : MaximizeIcon;
  const terminalFullscreenLabel = terminalChrome.exitFullscreen
    ? t('workbench:mobile.terminalPanel.exitFullscreen')
    : t('workbench:mobile.terminalPanel.enterFullscreen');

  // Business Logic: 切换项目/worktree 后不得把旧 commit unknown 锁带到新上下文。
  /* eslint-disable react-hooks/set-state-in-effect -- context 切换时必须同步清空 phase */
  useEffect(() => {
    commitContextRef.current = project && worktree
      ? { projectId: project.id, worktreeId: worktree.id }
      : null;
    setCommitPhase('idle');
    setMergePhase('idle');
    setActionBusy(null);
    setPanelError(null);
    setHookRepair(null);
    setCommitSuccess(null);
    setSelectionCopied(null);
    setFabMenuOpen(false);
    commitOperationIdRef.current = null;
    mergeOperationIdRef.current = null;
    // eslint-disable-next-line react-hooks/exhaustive-deps -- 仅 id 驱动重置
  }, [project?.id, worktree?.id]);
  /* eslint-enable react-hooks/set-state-in-effect */

  /* eslint-disable react-hooks/set-state-in-effect -- 切 session 必须收起 FAB，避免动作打到新窗口 */
  useEffect(() => {
    setFabMenuOpen(false);
  }, [sessionId]);
  /* eslint-enable react-hooks/set-state-in-effect */

  useEffect(() => {
    if (!fabMenuOpen) return undefined;
    const handleKeyDown = (event: KeyboardEvent): void => {
      if (event.key !== 'Escape') return;
      event.preventDefault();
      setFabMenuOpen(false);
    };
    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, [fabMenuOpen]);

  useEffect(() => {
    inputEnabledRef.current = Boolean(
      sessionId &&
        visibleSession?.status === 'running' &&
        !busy &&
        inputStreamState.status === 'ready',
    );
  }, [busy, inputStreamState.status, sessionId, visibleSession?.status]);

  useEffect(() => {
    stickyModifierRef.current = stickyModifier;
  }, [stickyModifier]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   sticky Ctrl/Alt 武装后若用户忘记再按键，应自动解除，避免后续普通输入被意外改写。
   *
   * Code Logic（这个函数做什么）:
   *   清掉旧 timer；武装时启动 3s timeout 将 sticky 置 null；disarm 只清 timer。
   */
  const armStickyModifier = useCallback((modifier: MobileTerminalStickyModifier | null): void => {
    if (stickyTimeoutRef.current !== null) {
      window.clearTimeout(stickyTimeoutRef.current);
      stickyTimeoutRef.current = null;
    }
    stickyModifierRef.current = modifier;
    setStickyModifier(modifier);
    if (!modifier) return;
    stickyTimeoutRef.current = window.setTimeout(() => {
      stickyTimeoutRef.current = null;
      stickyModifierRef.current = null;
      setStickyModifier(null);
    }, MOBILE_TERMINAL_STICKY_TIMEOUT_MS);
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   Extra keys 与 xterm onData 共用同一输入流；失败时需要同一错误面板文案。
   *
   * Code Logic（这个函数做什么）:
   *   在 input 可用时 enqueue；捕获异常写入 panelError。
   */
  const sendTerminalInput = useCallback(
    (data: string): void => {
      if (!sessionId || !inputEnabledRef.current) return;
      try {
        inputStreamRef.current?.enqueue(sessionId, data);
      } catch (reason) {
        setPanelError(
          `${t('workbench:mobile.terminalPanel.errors.write')}: ${getErrorMessage(
            reason,
            t('workbench:errors.sessions'),
          )}`,
        );
      }
    },
    [sessionId, t],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   复制成功、取消、Esc 或换 session 时必须退出划选，避免底栏残留并挡住后续滚动。
   *
   * Code Logic（这个函数做什么）:
   *   清长按 timer、复位手势 ref、clearSelection，并把 selecting 置 false。
   */
  const exitSelecting = useCallback((): void => {
    selectingRef.current = false;
    setSelecting(false);
    setSelectionEmpty(true);
    terminalRef.current?.clearSelection();
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   底栏复制要把 xterm 选区写入手机剪贴板，且不得写 PTY；失败时必须留在划选以便重试。
   *
   * Code Logic（这个函数做什么）:
   *   空选区直接返回；writeClipboardText 成功则短暂成功提示并 exitSelecting，失败写入 panelError。
   */
  const handleCopySelection = useCallback(async (): Promise<void> => {
    const terminal = terminalRef.current;
    if (!terminal) return;
    const text = terminal.getSelection();
    if (text.length === 0) return;
    const result = await writeClipboardText(text);
    if (result.ok) {
      setPanelError(null);
      setSelectionCopied(t('workbench:mobile.terminalPanel.selection.copied'));
      exitSelecting();
      return;
    }
    setPanelError(t('workbench:mobile.terminalPanel.selection.copyFailed'));
  }, [exitSelecting, t]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   相册选出图片后必须立刻走现有 paste-image 通道，不能预览或把图塞进输入帧。
   *
   * Code Logic（这个函数做什么）:
   *   读 file input 的首个文件并清空 value；busy/会话不可写时忽略；fileToPngDataUrl 后 pasteImage。
   */
  const handlePasteImageFileChange = useCallback(
    (event: ChangeEvent<HTMLInputElement>): void => {
      const file = event.target.files?.[0] ?? null;
      event.target.value = '';
      if (
        !file ||
        !sessionId ||
        pasteImageBusyRef.current ||
        visibleSession?.status !== 'running' ||
        busy ||
        inputStreamState.status !== 'ready'
      ) {
        return;
      }
      pasteImageBusyRef.current = true;
      setPasteImageBusy(true);
      void fileToPngDataUrl(file)
        .then((dataUrl) => httpWorkbenchTransport.sessions.pasteImage(sessionId, dataUrl))
        .catch((reason) => {
          setPanelError(
            `${t('workbench:errors.pasteImage')}: ${getErrorMessage(
              reason,
              t('workbench:errors.pasteImage'),
            )}`,
          );
        })
        .finally(() => {
          pasteImageBusyRef.current = false;
          setPasteImageBusy(false);
        });
    },
    [busy, inputStreamState.status, sessionId, t, visibleSession?.status],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   收藏 Prompt 面板选中条目后，需要把 prompt 内容写入当前终端输入行（不回车），
   *   与桌面端快捷键浮层语义一致，让用户接着编辑或自行 Enter。
   *
   * Code Logic（这个函数做什么）:
   *   有 active sessionId 时复用 sendTerminalInput（自带 inputEnabled 门闩），
   *   不拼 \r；写入失败由 sendTerminalInput 统一投影 panelError。
   */
  const handleSelectFavoritePrompt = useCallback(
    (prompt: Prompt): void => {
      if (!sessionId) return;
      sendTerminalInput(prompt.content);
    },
    [sendTerminalInput, sessionId],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   终端右侧需要与桌面 Git 历史 Commit 同口径的一键提交，不离开终端、不弹手写 message。
   *
   * Code Logic（这个函数做什么）:
   *   稳定 clientOperationId + executeMobileGitCommit(message=null)；unknown 只对账不盲重放。
   */
  const handleCommitWorktree = useCallback(async (): Promise<void> => {
    if (busy || !project || !worktree) return;
    if (isMobileMutationActionLocked(commitPhase) && commitPhase !== 'unknown') return;
    const actionContext = { projectId: project.id, worktreeId: worktree.id };
    const clientOperationId = pickMobileMutationOperationId(
      commitPhase,
      commitOperationIdRef.current,
      createHttpOrchestratorClientRequestId(),
    );
    commitOperationIdRef.current = clientOperationId;
    setActionBusy('commit');
    setCommitPhase('busy');
    setPanelError(null);
    setCommitSuccess(null);
    setHookRepair(null);
    try {
      const outcome = await executeMobileGitCommit({
        worktreeId: worktree.id,
        clientOperationId,
        reconcileOnly: commitPhase === 'unknown',
        isCurrent: () =>
          isMobileGitActionResponseCurrent(actionContext, commitContextRef.current),
        git: workbenchHttp.git,
      });
      if (outcome.type === 'stale') return;
      if (outcome.type === 'succeeded') {
        commitOperationIdRef.current = null;
        setCommitPhase('idle');
        setCommitSuccess(t('workbench:mobile.gitPanel.commitSucceeded'));
        onWorktreeChange?.(outcome.worktree);
        await onRefreshWorktrees?.();
        return;
      }
      if (outcome.type === 'succeededRefresh') {
        commitOperationIdRef.current = null;
        setCommitPhase('idle');
        setCommitSuccess(t('workbench:mobile.gitPanel.commitSucceeded'));
        await onRefreshWorktrees?.();
        return;
      }
      if (outcome.type === 'failedHook') {
        commitOperationIdRef.current = null;
        setCommitPhase('idle');
        setHookRepair({
          kind: 'commit',
          hookFailure: outcome.hookFailure,
          clientOperationId,
        });
        return;
      }
      if (outcome.type === 'failed') {
        commitOperationIdRef.current = null;
        setCommitPhase('idle');
        setPanelError(t('workbench:errors.mutationFailed'));
        return;
      }
      setCommitPhase('unknown');
      setPanelError(t('workbench:errors.mutationUnknown'));
    } catch (reason) {
      if (!isMobileGitActionResponseCurrent(actionContext, commitContextRef.current)) return;
      setCommitPhase('idle');
      commitOperationIdRef.current = null;
      setPanelError(
        `${t('workbench:errors.commitWorktree')}: ${getErrorMessage(
          reason,
          t('workbench:errors.commitWorktree'),
        )}`,
      );
    } finally {
      if (isMobileGitActionResponseCurrent(actionContext, commitContextRef.current)) {
        setActionBusy(null);
      }
    }
  }, [
    busy,
    commitPhase,
    onRefreshWorktrees,
    onWorktreeChange,
    project,
    t,
    worktree,
  ]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   终端右侧需要与桌面 Git 历史 Merge 同口径的一键合并，不离开终端。
   *
   * Code Logic（这个函数做什么）:
   *   委托父级 dirty guard / envelope merge；unknown 只查询 ledger 对账，禁止新 ID 盲重放。
   */
  const handleMergeWorktree = useCallback(async (): Promise<void> => {
    if (busy || !project || !worktree || !onMergeWorktree) return;
    if (!canShowMobileTerminalMergeFab(worktree)) return;
    if (isMobileMutationActionLocked(commitPhase)) return;
    if (isMobileMutationActionLocked(mergePhase) && mergePhase !== 'unknown') return;
    const actionContext = { projectId: project.id, worktreeId: worktree.id };
    setActionBusy('merge');
    setMergePhase('busy');
    setPanelError(null);
    setCommitSuccess(null);
    try {
      if (mergePhase === 'unknown' && mergeOperationIdRef.current) {
        const ledger = await workbenchHttp.git
          .getMutationOperation(mergeOperationIdRef.current)
          .catch(() => null);
        if (!isMobileGitActionResponseCurrent(actionContext, commitContextRef.current)) return;
        if (ledger?.state === 'succeeded') {
          mergeOperationIdRef.current = null;
          setMergePhase('idle');
          await onRefreshWorktrees?.({ expectedProjectId: actionContext.projectId });
          await onRefreshSessions?.();
          return;
        }
        if (ledger?.state === 'failed') {
          mergeOperationIdRef.current = null;
          setMergePhase('idle');
          setPanelError(t('workbench:errors.mutationFailed'));
          return;
        }
        setMergePhase('unknown');
        setPanelError(t('workbench:errors.mutationUnknown'));
        return;
      }

      const didMerge = await onMergeWorktree(worktree);
      if (!didMerge) {
        setMergePhase('idle');
        return;
      }
      // merge 会关闭源 worktree 会话；即便随后切到 main 使 merge context 过期，也必须刷新权威列表。
      await onRefreshSessions?.();
      if (!isMobileGitMergeResponseCurrent(actionContext, commitContextRef.current)) return;
      mergeOperationIdRef.current = null;
      setMergePhase('idle');
    } catch (reason) {
      if (!isMobileGitActionResponseCurrent(actionContext, commitContextRef.current)) return;
      if (isWorkbenchMutationUnknownError(reason)) {
        mergeOperationIdRef.current = getUnknownMutationClientOperationId(reason);
        setMergePhase('unknown');
        setPanelError(t('workbench:errors.mutationUnknown'));
      } else {
        setMergePhase('idle');
        mergeOperationIdRef.current = null;
        setPanelError(
          `${t('workbench:errors.mergeWorktree')}: ${getErrorMessage(
            reason,
            t('workbench:errors.mergeWorktree'),
          )}`,
        );
      }
    } finally {
      if (isMobileGitActionResponseCurrent(actionContext, commitContextRef.current)) {
        setActionBusy(null);
      }
    }
  }, [
    busy,
    commitPhase,
    mergePhase,
    onMergeWorktree,
    onRefreshSessions,
    onRefreshWorktrees,
    project,
    t,
    worktree,
  ]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   终端 FAB 在 failedHook 后需要与桌面相同的「让 AI 修复」入口。
   *
   * Code Logic（这个函数做什么）:
   *   调 repairHookFailure；成功后写入 terminalSessionId 并聚焦新终端。
   */
  const handleRepairHookFailure = useCallback(async (): Promise<void> => {
    if (busy || !project || !worktree || !hookRepair) return;
    const actionContext = { projectId: project.id, worktreeId: worktree.id };
    setActionBusy('commit');
    setPanelError(null);
    setCommitSuccess(null);
    try {
      const repair = await workbenchHttp.git.repairHookFailure(
        worktree.id,
        hookRepair.hookFailure,
      );
      if (!isMobileGitActionResponseCurrent(actionContext, commitContextRef.current)) return;
      setHookRepair({ ...hookRepair, terminalSessionId: repair.terminalSessionId });
      await onFocusRepairSession?.(repair.terminalSessionId);
    } catch (reason) {
      if (!isMobileGitActionResponseCurrent(actionContext, commitContextRef.current)) return;
      setPanelError(
        `${t('workbench:errors.commitWorktree')}: ${getErrorMessage(
          reason,
          t('workbench:errors.commitWorktree'),
        )}`,
      );
    } finally {
      if (isMobileGitActionResponseCurrent(actionContext, commitContextRef.current)) {
        setActionBusy(null);
      }
    }
  }, [busy, hookRepair, onFocusRepairSession, project, t, worktree]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户放弃当前钩子失败上下文时，不应再挡在终端上。
   *
   * Code Logic（这个函数做什么）:
   *   清空 hookRepair。
   */
  const handleDismissHookFailure = useCallback((): void => {
    setHookRepair(null);
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   AI 修复完成后用户从终端 FAB 重试 commit。
   *
   * Code Logic（这个函数做什么）:
   *   清空 hookRepair 后再次走 handleCommitWorktree。
   */
  const handleRetryAfterRepair = useCallback((): void => {
    setHookRepair(null);
    void handleCommitWorktree();
  }, [handleCommitWorktree]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   额外键条的 payload/modifier 动作需要在面板层统一消化。
   *
   * Code Logic（这个函数做什么）:
   *   resolve 键定义 → send / toggle sticky；payload 发送不消耗 sticky（与 Termux 独立宏键一致）。
   */
  const handleExtraKeyPress = useCallback(
    (key: MobileTerminalExtraKeyDef): void => {
      if (key.id === 'esc' && selectingRef.current) {
        exitSelecting();
        return;
      }
      // 按 extra key 只发送按键，不 blur 终端 helper textarea：避免输入态下打乱 xterm 输入追踪
      // （已输入内容被重复发送）。焦点保持在终端，软键盘由用户点击终端外区域收起。
      const action = resolveMobileTerminalExtraKeyPress(key);
      if (action.type === 'send') {
        sendTerminalInput(action.data);
        return;
      }
      if (action.type === 'toggleModifier') {
        const next = toggleStickyModifier(stickyModifierRef.current, action.modifier);
        armStickyModifier(next.type === 'arm' ? next.modifier : null);
        return;
      }
    },
    [armStickyModifier, exitSelecting, sendTerminalInput],
  );

  // 终端输入常驻 WebSocket 在面板挂载时建立一次(依赖 [])。StrictMode(dev)会双调用本 effect:
  // 首个 stream 在 CONNECTING 阶段就被 cleanup 的 stream.close() 中止,浏览器对中止未完成的
  // upgrade 会触发 error 事件;若放任其 onStateChange 继续 setState,会污染随后 ready stream
  // 的展示(残留"终端输入连接失败")。用 active 标志守卫:cleanup 后被废弃 stream 的事件不再写 state。
  useEffect(() => {
    let active = true;
    const stream = new MobileTerminalInputStream({
      onStateChange: (state) => {
        if (!active) return;
        setInputStreamState(state);
        if (state.status === 'blocked') {
          setPanelError(state.message);
        } else if (state.status === 'ready') {
          // 连接(重)建立后清掉历史 blocked 错误,避免 ready 后仍显示"终端输入连接失败"。
          setPanelError(null);
        }
      },
    });
    inputStreamRef.current = stream;
    return () => {
      active = false;
      inputStreamRef.current = null;
      stream.close();
      if (stickyTimeoutRef.current !== null) {
        window.clearTimeout(stickyTimeoutRef.current);
        stickyTimeoutRef.current = null;
      }
    };
  }, []);

  useEffect(() => {
    if (!project) {
      if (activeSession) onActiveSessionChange(null);
      return;
    }
    if (visibleSession) return;
    const nextSession = selectPreferredMobileSession(scopedSessions, worktree?.id ?? null);
    if ((nextSession?.id ?? null) !== (activeSession?.id ?? null)) {
      onActiveSessionChange(nextSession);
    }
  }, [
    activeSession,
    onActiveSessionChange,
    project,
    scopedSessions,
    visibleSession,
    worktree?.id,
  ]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   app tab 选中态必须同步到底层 tmux current window，避免手机 UI 与真实 tmux window 分裂。
   *
   * Code Logic（这个函数做什么）:
   *   调用 HTTP focus route；成功后记录已聚焦 session，失败时清空去重基线并把本地化错误写入面板错误区。
   */
  const focusSessionById = useCallback(
    async (nextSessionId: string): Promise<void> => {
      try {
        await httpWorkbenchTransport.sessions.focus(nextSessionId);
        lastFocusedSessionIdRef.current = nextSessionId;
      } catch (reason) {
        lastFocusedSessionIdRef.current = null;
        setPanelError(
          `${t('workbench:mobile.terminalPanel.errors.focus')}: ${getErrorMessage(
            reason,
            t('workbench:errors.focusSession'),
          )}`,
        );
      }
    },
    [t],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   移动端屏幕只能高效操作单个 pane，进入多 pane window 后必须隐藏 tmux 分屏布局。
   *
   * Code Logic（这个函数做什么）:
   *   仅对 running tmux-backed session 调用 HTTP zoom-pane route；后端会幂等判断 paneCount/zoom 状态。
   */
  const ensurePaneZoomedById = useCallback(
    async (session: WorkbenchSession): Promise<void> => {
      if (session.status !== 'running' || !session.supportsPanes) return;
      try {
        await httpWorkbenchTransport.sessions.zoomPane(session.id);
      } catch (reason) {
        setPanelError(
          `${t('workbench:mobile.terminalPanel.errors.zoomPane')}: ${getErrorMessage(
            reason,
            t('workbench:errors.sessions'),
          )}`,
        );
      }
    },
    [t],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   移动端切换 terminal window 时，前端 active session、tmux current window 和单 pane 展示状态必须一起同步。
   *
   * Code Logic（这个函数做什么）:
   *   先调用 focus 同步 tmux window，再按 session 能力调用 zoom-pane 确保当前 active pane 全屏显示。
   */
  const focusSessionAndZoomById = useCallback(
    async (session: WorkbenchSession): Promise<void> => {
      if (session.status !== 'running') return;
      await focusSessionById(session.id);
      await ensurePaneZoomedById(session);
    },
    [ensurePaneZoomedById, focusSessionById],
  );

  useEffect(() => {
    if (!sessionId) {
      lastFocusedSessionIdRef.current = null;
      return;
    }
    if (lastFocusedSessionIdRef.current === sessionId) {
      if (visibleSession) {
        queueMicrotask(() => {
          void ensurePaneZoomedById(visibleSession);
        });
      }
      return;
    }
    if (!visibleSession || visibleSession.status !== 'running') return;
    queueMicrotask(() => {
      void focusSessionAndZoomById(visibleSession);
    });
  }, [ensurePaneZoomedById, focusSessionAndZoomById, sessionId, visibleSession]);



  /**
   * Business Logic（为什么需要这个函数）:
   *   用户点击 terminal window tab 时，移动端状态栏、xterm 和底层 tmux current window 都要切换到同一个 session。
   *
   * Code Logic（这个函数做什么）:
   *   写入父组件 active session，并调用 focus HTTP route；错误由 focusSessionById 展示。
   */
  const handleSelectSession = useCallback(
    (session: WorkbenchSession): void => {
      onActiveSessionChange(session);
      void focusSessionAndZoomById(session);
    },
    [focusSessionAndZoomById, onActiveSessionChange],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   手机端需要能在当前项目/worktree 中创建真实 terminal window，而不是渲染假的前端 tab。
   *
   * Code Logic（这个函数做什么）:
   *   测量当前终端区域尺寸，调用 HTTP create route，追加 session、重置 buffer、设为 active，并刷新权威 session 列表。
   */
  const handleCreateSession = useCallback(async (): Promise<void> => {
    if (!project) return;
    setActionBusy('create');
    setPanelError(null);
    try {
      // Prefer the terminal viewport (excludes extra-keys bar) so initial PTY size matches fit area.
      const initialSize = measureMobileTerminalSize(
        viewportRef.current ?? surfaceRef.current,
      );
      const session = await httpWorkbenchTransport.sessions.create(
        project.id,
        initialSize,
        worktree?.id ?? null,
      );
      const nextSessions = [...sessions.filter((item) => item.id !== session.id), session];
      onSessionsChange(nextSessions);
      resetBuffer(session.id);
      onActiveSessionChange(session);
      await focusSessionAndZoomById(session);
      await onRefreshSessions?.();
    } catch (reason) {
      setPanelError(
        `${t('workbench:mobile.terminalPanel.errors.create')}: ${getErrorMessage(
          reason,
          t('workbench:errors.createSession'),
        )}`,
      );
    } finally {
      setActionBusy(null);
    }
  }, [
    focusSessionAndZoomById,
    onActiveSessionChange,
    onRefreshSessions,
    onSessionsChange,
    project,
    resetBuffer,
    sessions,
    t,
    worktree,
  ]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   手机端新增 pane 必须由 tmux 创建真实 pane，但不需要让用户选择左右/上下分屏方向。
   *
   * Code Logic（这个函数做什么）:
   *   使用移动端固定 split 方向调用 HTTP split-pane route，完成后刷新 session 列表以同步 paneCount。
   */
  const handleCreatePane = useCallback(
    async (): Promise<void> => {
      if (!visibleSession) return;
      setActionBusy('create-pane');
      setPanelError(null);
      try {
        const direction = getMobileCreatePaneDirection();
        await httpWorkbenchTransport.sessions.splitPane(visibleSession.id, direction);
        await ensurePaneZoomedById(visibleSession);
        await onRefreshSessions?.();
      } catch (reason) {
        setPanelError(
          `${t('workbench:mobile.terminalPanel.errors.split')}: ${getErrorMessage(
            reason,
            t('workbench:errors.splitPane'),
          )}`,
        );
      } finally {
        setActionBusy(null);
      }
    },
    [ensurePaneZoomedById, onRefreshSessions, t, visibleSession],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   手机端用户无法方便地按 tmux 快捷键，工具栏需要一键切到当前 window 的下一个 pane。
   *
   * Code Logic（这个函数做什么）:
   *   调用 HTTP switch-pane route；该操作不改变 session 列表，只让后端 tmux 选择下一个 active pane。
   */
  const handleSwitchPane = useCallback(async (): Promise<void> => {
    if (!visibleSession) return;
    setActionBusy('switch-pane');
    setPanelError(null);
    try {
      await httpWorkbenchTransport.sessions.switchPane(visibleSession.id);
      await ensurePaneZoomedById(visibleSession);
    } catch (reason) {
      setPanelError(
        `${t('workbench:mobile.terminalPanel.errors.switchPane')}: ${getErrorMessage(
          reason,
          t('workbench:errors.sessions'),
        )}`,
      );
    } finally {
      setActionBusy(null);
    }
  }, [ensurePaneZoomedById, t, visibleSession]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   关闭 pane 应映射到真实 tmux pane；关闭最后一个 pane 时要移除对应 terminal window tab 和 buffer。
   *
   * Code Logic（这个函数做什么）:
   *   调用 HTTP close-pane route；closedWindow=true 时本地先移除 session 并选择同 worktree 的下一个优先 session，再刷新权威列表。
   */
  const handleClosePane = useCallback(async (): Promise<void> => {
    if (!visibleSession) return;
    setActionBusy('close-pane');
    setPanelError(null);
    try {
      const result = await httpWorkbenchTransport.sessions.closePane(visibleSession.id);
      if (result.closedWindow) {
        const nextSessions = sessions.filter((session) => session.id !== result.sessionId);
        onSessionsChange(nextSessions);
        removeBuffer(result.sessionId);
        onActiveSessionChange(selectPreferredMobileSession(nextSessions, worktree?.id ?? null));
      } else {
        await ensurePaneZoomedById(visibleSession);
      }
      await onRefreshSessions?.();
    } catch (reason) {
      setPanelError(
        `${t('workbench:mobile.terminalPanel.errors.closePane')}: ${getErrorMessage(
          reason,
          t('workbench:errors.closePane'),
        )}`,
      );
    } finally {
      setActionBusy(null);
    }
  }, [
    onActiveSessionChange,
    onRefreshSessions,
    onSessionsChange,
    ensurePaneZoomedById,
    removeBuffer,
    sessions,
    t,
    visibleSession,
    worktree,
  ]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   手机端也需要能关闭当前 terminal window，释放后端 PTY/tmux attach 与前端 buffer。
   *
   * Code Logic（这个函数做什么）:
   *   调用 HTTP close route，移除本地 session 与缓存，并选择当前 worktree 下的下一个优先 session。
   */
  const handleCloseSession = useCallback(
    async (session: WorkbenchSession): Promise<void> => {
      setActionBusy(`close-${session.id}`);
      setPanelError(null);
      try {
        await httpWorkbenchTransport.sessions.close(session.id);
        const nextSessions = sessions.filter((item) => item.id !== session.id);
        onSessionsChange(nextSessions);
        removeBuffer(session.id);
        if (activeSession?.id === session.id) {
          onActiveSessionChange(selectPreferredMobileSession(nextSessions, worktree?.id ?? null));
        }
        await onRefreshSessions?.();
      } catch (reason) {
        setPanelError(
          `${t('workbench:mobile.terminalPanel.errors.closeWindow')}: ${getErrorMessage(
            reason,
            t('workbench:errors.closeSession'),
          )}`,
        );
      } finally {
        setActionBusy(null);
      }
    },
    [
      activeSession,
      onActiveSessionChange,
      onRefreshSessions,
      onSessionsChange,
      removeBuffer,
      sessions,
      t,
      worktree,
    ],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   手机屏幕空间有限，用户需要把当前终端临时铺满屏幕，并隐藏移动端 shell 与 window tabs。
   *
   * Code Logic（这个函数做什么）:
   *   有可见 session 时打开全屏状态；没有终端 window 时忽略，避免空态占满屏幕。
   */
  const handleEnterTerminalFullscreen = useCallback((): void => {
    if (!visibleSession) return;
    setTerminalFullscreen(true);
  }, [visibleSession]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   进入全屏后必须保留明确退出入口，让用户回到完整移动端 Workbench shell。
   *
   * Code Logic（这个函数做什么）:
   *   关闭本地全屏状态，触发 CSS 从 fixed overlay 回到普通面板布局。
   */
  const handleExitTerminalFullscreen = useCallback((): void => {
    setTerminalFullscreen(false);
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   选贴图后应收起环形菜单，再打开系统相册。
   *
   * Code Logic（这个函数做什么）:
   *   关闭 FAB 菜单并触发隐藏 file input 的 click。
   */
  const handleFabPasteImage = useCallback((): void => {
    setFabMenuOpen(false);
    pasteImageInputRef.current?.click();
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   环形菜单上的 Merge 与原先 FAB 同口径，选完即收起以免挡终端。
   *
   * Code Logic（这个函数做什么）:
   *   关闭菜单后调用 handleMergeWorktree。
   */
  const handleFabMerge = useCallback((): void => {
    setFabMenuOpen(false);
    void handleMergeWorktree();
  }, [handleMergeWorktree]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   环形菜单上的 Commit 与原先 FAB 同口径，选完即收起。
   *
   * Code Logic（这个函数做什么）:
   *   关闭菜单后调用 handleCommitWorktree。
   */
  const handleFabCommit = useCallback((): void => {
    setFabMenuOpen(false);
    void handleCommitWorktree();
  }, [handleCommitWorktree]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   Prompt 优化走浮层，打开前先收起环形动作以免叠层。
   *
   * Code Logic（这个函数做什么）:
   *   关闭菜单并打开优化 sheet。
   */
  const handleFabOpenOptimizer = useCallback((): void => {
    setFabMenuOpen(false);
    setPromptOptimizerSheetOpen(true);
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   收藏 Prompt 走浮层，打开前先收起环形动作。
   *
   * Code Logic（这个函数做什么）:
   *   关闭菜单并打开收藏快捷输入。
   */
  const handleFabOpenFavorite = useCallback((): void => {
    setFabMenuOpen(false);
    setFavoriteSheetOpen(true);
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   FAB 叠在 xterm 上。PointerPrimaryButton 为了不丢终端焦点会 preventDefault，
   *   手机就会把这次 tap 当成继续编辑 helper textarea，弹出系统键盘。
   *
   * Code Logic（这个函数做什么）:
   *   对当前 viewport 的 helper textarea 设 readonly + inputmode=none 并 blur，
   *   同时 blur 当前可编辑 activeElement。
   */
  const dismissTerminalSoftKeyboard = useCallback((): void => {
    const active =
      typeof document !== 'undefined' && document.activeElement instanceof HTMLElement
        ? document.activeElement
        : null;
    leaveMobileTerminalTypingMode(
      findMobileTerminalHelperTextarea(viewportRef.current),
      active,
    );
  }, []);

  type TerminalFabAction = {
    key: 'paste' | 'merge' | 'commit' | 'optimizer' | 'favorite';
    visible: boolean;
    label: string;
    disabled: boolean;
    busy: boolean;
    dirty: boolean;
    icon: ReactNode;
  };
  const allTerminalFabActions: TerminalFabAction[] = [
    {
      key: 'paste',
      visible: true,
      label: t('workbench:mobile.terminalPanel.pasteImageButton'),
      disabled: !canPasteImage || pasteImageBusy,
      busy: pasteImageBusy,
      dirty: false,
      icon: <ImageIcon size={18} aria-hidden="true" />,
    },
    {
      key: 'merge',
      visible: showMergeFab,
      label: mergeLabel,
      disabled: !canMergeWorktree || mergeBusy,
      busy: mergeBusy,
      dirty: false,
      icon: <SyncIcon size={18} aria-hidden="true" />,
    },
    {
      key: 'commit',
      visible: true,
      label: commitLabel,
      disabled: !canCommitWorktree || commitBusy,
      busy: commitBusy,
      dirty: worktreeDirty,
      icon: <CommitIcon size={18} aria-hidden="true" />,
    },
    {
      key: 'optimizer',
      visible: true,
      label: t('workbench:promptOptimizer.open'),
      disabled: !canOpenFavoriteQuickInput,
      busy: false,
      dirty: false,
      icon: <EditIcon size={18} aria-hidden="true" />,
    },
    {
      key: 'favorite',
      visible: true,
      label: t('workbench:mobile.favoriteQuickInput.openButton'),
      disabled: !canOpenFavoriteQuickInput,
      busy: false,
      dirty: false,
      icon: <PromptsIcon size={18} aria-hidden="true" />,
    },
  ];
  const terminalFabActions = allTerminalFabActions.filter((item) => item.visible);
  const terminalFabArcPath = computeMobileTerminalFabArcPath(terminalFabActions.length);

  const terminalBody = !project ? (
    <div className={styles.mobileTerminalEmpty}>
      {t('workbench:mobile.terminalPanel.noProject')}
    </div>
  ) : !visibleSession ? (
    <div className={styles.mobileTerminalEmpty}>
      <span>{t('workbench:mobile.terminalPanel.noSession')}</span>
      <PointerPrimaryButton
        type="button"
        className={styles.mobileTerminalPrimaryButton}
        disabled={isActionDisabled}
        onPrimary={() => void handleCreateSession()}
      >
        <PlusIcon size={16} aria-hidden="true" />
        <span>{t('workbench:mobile.terminalPanel.newWindow')}</span>
      </PointerPrimaryButton>
    </div>
  ) : null;

  return (
    <section
      className={`${styles.panel} ${styles.mobileTerminalPanel}`}
      data-fullscreen={isTerminalFullscreen || undefined}
    >
      {terminalChrome.worktreeStrip && worktreeBar ? (
        <div
          className={styles.mobileTerminalWorktreeSlot}
          onPointerDownCapture={dismissTerminalSoftKeyboard}
        >
          <MobileWorktreeTabs {...worktreeBar} />
        </div>
      ) : null}

      <div
        className={styles.mobileTerminalToolbar}
        data-fullscreen={isTerminalFullscreen || undefined}
      >
        {terminalChrome.windowTabs ? (
          <div
            className={styles.mobileTerminalTabs}
            role="tablist"
            aria-label={t('workbench:mobile.terminalPanel.tabsAriaLabel')}
            onPointerDownCapture={dismissTerminalSoftKeyboard}
          >
            {scopedSessions.map((session) => {
              const isActive = session.id === visibleSession?.id;
              return (
                <div
                  key={session.id}
                  className={styles.mobileSessionTab}
                  data-active={isActive || undefined}
                >
                  <PointerPrimaryButton
                    type="button"
                    role="tab"
                    aria-selected={isActive}
                    className={styles.mobileSessionSelectButton}
                    onPrimary={() => handleSelectSession(session)}
                  >
                    <span className={styles.mobileSessionDot} data-status={session.status} />
                    <span className={styles.mobileSessionName}>{session.name}</span>
                    <span className={styles.mobileSessionPaneCount}>
                      {t('workbench:mobile.terminalPanel.paneCount', {
                        count: session.paneCount,
                      })}
                    </span>
                  </PointerPrimaryButton>
                  <PointerPrimaryButton
                    type="button"
                    className={styles.mobileTerminalTabClose}
                    aria-label={t('workbench:mobile.terminalPanel.closeWindow')}
                    disabled={isActionDisabled}
                    onPrimary={(event) => {
                      event.stopPropagation();
                      void handleCloseSession(session);
                    }}
                  >
                    <XIcon size={14} aria-hidden="true" />
                  </PointerPrimaryButton>
                </div>
              );
            })}
            <PointerPrimaryButton
              type="button"
              className={styles.mobileTerminalPrimaryButton}
              disabled={!project || isActionDisabled}
              onPrimary={() => void handleCreateSession()}
            >
              <PlusIcon size={16} aria-hidden="true" />
              <span>{t('workbench:mobile.terminalPanel.newWindow')}</span>
            </PointerPrimaryButton>
          </div>
        ) : null}

        {terminalChrome.paneActions ? (
          <div
            className={styles.mobileTerminalActions}
            aria-label={t('workbench:mobile.terminalPanel.actionsAriaLabel')}
            onPointerDownCapture={dismissTerminalSoftKeyboard}
          >
            <PointerPrimaryButton
              type="button"
              className={styles.mobileTerminalActionButton}
              disabled={!canUsePaneActions}
              aria-label={t('workbench:mobile.terminalPanel.addPane')}
              title={t('workbench:mobile.terminalPanel.addPane')}
              onPrimary={() => void handleCreatePane()}
            >
              <PlusIcon size={16} aria-hidden="true" />
              <span>{t('workbench:mobile.terminalPanel.addPane')}</span>
            </PointerPrimaryButton>
            <PointerPrimaryButton
              type="button"
              className={styles.mobileTerminalActionButton}
              disabled={!canSwitchPane}
              aria-label={t('workbench:mobile.terminalPanel.switchPane')}
              title={t('workbench:mobile.terminalPanel.switchPane')}
              onPrimary={() => void handleSwitchPane()}
            >
              <ArrowRightIcon size={16} aria-hidden="true" />
              <span>{t('workbench:mobile.terminalPanel.switchPane')}</span>
            </PointerPrimaryButton>
            <PointerPrimaryButton
              type="button"
              className={styles.mobileTerminalActionButton}
              disabled={!canUsePaneActions}
              aria-label={t('workbench:mobile.terminalPanel.closePane')}
              title={t('workbench:mobile.terminalPanel.closePane')}
              onPrimary={() => void handleClosePane()}
            >
              <XIcon size={16} aria-hidden="true" />
              <span>{t('workbench:mobile.terminalPanel.closePane')}</span>
            </PointerPrimaryButton>
            <PointerPrimaryButton
              type="button"
              className={styles.mobileTerminalActionButton}
              disabled={!terminalChrome.exitFullscreen && !visibleSession}
              aria-label={terminalFullscreenLabel}
              title={terminalFullscreenLabel}
              onPrimary={() =>
                terminalChrome.exitFullscreen
                  ? handleExitTerminalFullscreen()
                  : handleEnterTerminalFullscreen()
              }
            >
              <TerminalFullscreenIcon size={16} aria-hidden="true" />
              <span>{terminalFullscreenLabel}</span>
            </PointerPrimaryButton>
          </div>
        ) : null}
      </div>

      {!isTerminalFullscreen && busy ? (
        <p className={styles.panelState}>{t('workbench:loading')}</p>
      ) : null}
      <div className={styles.mobileTerminalAlerts}>
        {commitSuccess ? (
          <StatusMessage tone="success" className={styles.panelState}>
            {commitSuccess}
          </StatusMessage>
        ) : null}
        {selectionCopied ? (
          <StatusMessage tone="success" className={styles.panelState}>
            {selectionCopied}
          </StatusMessage>
        ) : null}
        {panelError ? (
          <p className={styles.panelError} role="alert">
            <span>{t('workbench:mobile.projectPanel.error')}</span>
            <span>{panelError}</span>
          </p>
        ) : null}
        {hookRepair ? (
          <MobileHookRepairCard
            hookRepair={hookRepair}
            busy={commitBusy || busy}
            onRepair={() => void handleRepairHookFailure()}
            onRetry={handleRetryAfterRepair}
            onDismiss={handleDismissHookFailure}
          />
        ) : null}
      </div>

      {terminalChrome.terminalSurface ? (
        <div
          className={styles.mobileTerminalSurface}
          ref={surfaceRef}
          aria-label={t('workbench:mobile.terminalPanel.terminalAriaLabel')}
        >
          {terminalBody}
          <div className={styles.mobileTerminalHostStack}>
            {mountedSessions.map((item) => (
              <MobileTerminalXtermSlot
                key={item.id}
                session={item}
                visible={item.id === sessionId}
                store={store}
                inputEnabled={
                  item.id === sessionId &&
                  item.status === 'running' &&
                  inputStreamState.status === 'ready' &&
                  !busy
                }
                activeAgentIdentity={
                  item.id === sessionId ? activeAgentIdentity : null
                }
                inputStreamRef={inputStreamRef}
                stickyModifierRef={stickyModifierRef}
                stickyTimeoutRef={stickyTimeoutRef}
                selectingRef={selectingRef}
                onError={setPanelError}
                onSelectingChange={(patch) => {
                  selectingRef.current = patch.selecting;
                  setSelecting(patch.selecting);
                  if (patch.selectedLineCount !== undefined) {
                    setSelectedLineCount(patch.selectedLineCount);
                  }
                  if (patch.selectionEmpty !== undefined) {
                    setSelectionEmpty(patch.selectionEmpty);
                  }
                  if (patch.clearCopied) setSelectionCopied(null);
                }}
                onBindSurface={(terminal, viewport) => {
                  if (!terminal) {
                    if (boundSessionIdRef.current === item.id) {
                      terminalRef.current = null;
                      viewportRef.current = null;
                      boundSessionIdRef.current = null;
                    }
                    return;
                  }
                  boundSessionIdRef.current = item.id;
                  terminalRef.current = terminal;
                  viewportRef.current = viewport;
                }}
                onStickyConsumed={() => setStickyModifier(null)}
              />
            ))}
          </div>
            {selecting ? (
              <div
                className={styles.mobileTerminalSelectionBar}
                role="toolbar"
                aria-label={t('workbench:mobile.terminalPanel.selection.barAriaLabel')}
                onPointerDownCapture={dismissTerminalSoftKeyboard}
              >
                <span className={styles.mobileTerminalSelectionMeta}>
                  {t('workbench:mobile.terminalPanel.selection.selectedLines', {
                    count: selectedLineCount,
                  })}
                </span>
                <PointerPrimaryButton
                  type="button"
                  className={styles.mobileTerminalSelectionAction}
                  disabled={selectionEmpty}
                  aria-label={t('workbench:mobile.terminalPanel.selection.copy')}
                  onPrimary={() => void handleCopySelection()}
                >
                  {t('workbench:mobile.terminalPanel.selection.copy')}
                </PointerPrimaryButton>
                <PointerPrimaryButton
                  type="button"
                  className={styles.mobileTerminalSelectionAction}
                  aria-label={t('workbench:mobile.terminalPanel.selection.cancel')}
                  onPrimary={exitSelecting}
                >
                  {t('workbench:mobile.terminalPanel.selection.cancel')}
                </PointerPrimaryButton>
              </div>
            ) : null}
          {visibleSession ? (
            <MobileTerminalExtraKeys
              disabled={
                !sessionId ||
                visibleSession.status !== 'running' ||
                busy ||
                inputStreamState.status !== 'ready'
              }
              stickyModifier={stickyModifier}
              onKeyPress={handleExtraKeyPress}
            />
          ) : null}
          {project ? (
            <div
              className={styles.mobileTerminalFabGroup}
              onPointerDownCapture={(event) => {
                event.preventDefault();
                dismissTerminalSoftKeyboard();
              }}
            >
                <input
                  ref={pasteImageInputRef}
                  type="file"
                  accept="image/*"
                  className={styles.mobileTerminalPasteImageInput}
                  tabIndex={-1}
                  aria-hidden="true"
                  onChange={handlePasteImageFileChange}
                />
                <div
                  id="mobile-terminal-fab-actions"
                  className={styles.mobileTerminalFabActions}
                  data-layout="radial"
                  data-open={fabMenuOpen || undefined}
                  data-count={terminalFabActions.length}
                  role="group"
                  aria-label={t('workbench:mobile.terminalPanel.fabMenu.actionsAriaLabel')}
                  aria-hidden={!fabMenuOpen}
                  inert={!fabMenuOpen || undefined}
                >
                  <svg
                    className={styles.mobileTerminalFabRing}
                    viewBox="-1.05 -1.05 2.1 2.1"
                    aria-hidden="true"
                  >
                    <path d={terminalFabArcPath} />
                  </svg>
                  {terminalFabActions.map((item, index) => {
                    const pose = computeMobileTerminalFabArc(index, terminalFabActions.length);
                    return (
                      <div
                        key={item.key}
                        className={styles.mobileTerminalFabAction}
                        data-fab-index={index}
                        data-fab-angle={pose.angleDeg}
                        style={
                          {
                            '--fab-angle': `${pose.angleDeg}deg`,
                            '--fab-delay-open': `${pose.delayOpenMs}ms`,
                            '--fab-delay-close': `${pose.delayCloseMs}ms`,
                          } as CSSProperties
                        }
                      >
                        <span className={styles.mobileTerminalFabLabel}>{item.label}</span>
                        <PointerPrimaryButton
                          type="button"
                          className={styles.mobileTerminalFab}
                          disabled={item.disabled}
                          data-dirty={item.dirty || undefined}
                          aria-busy={item.busy || undefined}
                          aria-label={item.label}
                          title={item.label}
                          onPrimary={() => {
                            if (item.key === 'paste') handleFabPasteImage();
                            else if (item.key === 'merge') handleFabMerge();
                            else if (item.key === 'commit') handleFabCommit();
                            else if (item.key === 'optimizer') handleFabOpenOptimizer();
                            else handleFabOpenFavorite();
                          }}
                        >
                          {item.icon}
                        </PointerPrimaryButton>
                      </div>
                    );
                  })}
                </div>
                <PointerPrimaryButton
                  type="button"
                  className={styles.mobileTerminalFab}
                  data-trigger="true"
                  data-open={fabMenuOpen || undefined}
                  aria-expanded={fabMenuOpen}
                  aria-controls="mobile-terminal-fab-actions"
                  aria-label={
                    fabMenuOpen
                      ? t('workbench:mobile.terminalPanel.fabMenu.close')
                      : t('workbench:mobile.terminalPanel.fabMenu.open')
                  }
                  title={
                    fabMenuOpen
                      ? t('workbench:mobile.terminalPanel.fabMenu.close')
                      : t('workbench:mobile.terminalPanel.fabMenu.open')
                  }
                  onPrimary={() => setFabMenuOpen((open) => !open)}
                >
                  {fabMenuOpen ? (
                    <XIcon size={18} aria-hidden="true" />
                  ) : (
                    <MoreIcon size={18} aria-hidden="true" />
                  )}
                </PointerPrimaryButton>
            </div>
          ) : null}
          {project ? (
            <button
              type="button"
              className={styles.mobileTerminalFabBackdrop}
              data-testid="mobile-terminal-fab-backdrop"
              data-open={fabMenuOpen || undefined}
              tabIndex={fabMenuOpen ? 0 : -1}
              aria-hidden={!fabMenuOpen}
              aria-label={t('workbench:mobile.terminalPanel.fabMenu.dismiss')}
              onPointerDown={dismissTerminalSoftKeyboard}
              onClick={() => setFabMenuOpen(false)}
            />
          ) : null}
        </div>
      ) : null}
      <MobileFavoriteQuickInput
        open={favoriteSheetOpen}
        onClose={() => setFavoriteSheetOpen(false)}
        onSelectPrompt={handleSelectFavoritePrompt}
      />
      <MobilePromptOptimizerSheet
        open={promptOptimizerSheetOpen}
        onClose={() => setPromptOptimizerSheetOpen(false)}
        worktree={worktree}
        session={activeSession}
      />
    </section>
  );
}
