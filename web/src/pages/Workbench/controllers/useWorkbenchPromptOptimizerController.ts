/**
 * Workbench Prompt 优化浮层 controller —— 配置加载、打开/输入/定位、Control 单键快捷键、
 * IME 安全、流式写入终端、焦点回归与重新打开清空状态。
 *
 * Business Logic（为什么需要这个 controller）:
 *   Workbench 内嵌的 Prompt 优化浮层是一套独立交互：用户按 Control 单键（或组合快捷键）唤起浮层，
 *   在浮层输入需求后按 Enter 让后端把优化结果流式写入当前活动终端。浮层定位跟随终端光标，重新打开
 *   必须清空上一次输入，IME 选词期间不能误触发提交，远端离线或无活动 session 时必须拦截并提示。
 *   这些行为原本散落在 Workbench.tsx 的多个 state / ref / effect / handler 里，本 controller 把它们
 *   集中持有，让页面只负责把状态渲染到 DOM、把 handler 绑到按钮/输入框。
 *
 *   重要边界（与其它 controller 一致）：
 *   - `automationConsoleOpen` 与 `workspaceView` 是跨域共享状态（终端全屏、自动化控制台、文件 tab 都会改写），
 *     仍归 Workbench.tsx 所有；controller 只读取它们来判断快捷键是否应生效。
 *   - controller 不持有 session 列表、worktree 列表或终端字节内容；这些仍归邻接 controller / 页面。
 *   - `activeSession` / `remoteWriteDisabled` / `activeProjectId` 由页面透传，仅用于读取。
 *   - 终端光标锚点 ref / prompt textarea ref 仍由页面持有（DOM ref 跨域共享），controller 通过注入的
 *     ref 读写最新值。
 *
 * Code Logic（这个 controller 做什么）:
 *   - 持有 `promptPanelOpen` / `promptInput` / `promptOptimizing` / `promptOptimizerHotkey` /
 *     `promptOptimizerFillLanguage` / `promptPanelPosition` state；
 *   - 持有 `promptShortcutStateRef` / `promptPanelOpenRef` ref（快捷键状态机 + 异步读最新开关态）；
 *   - 注册 configApi 加载 effect（mount 时拉取 hotkey / fillLanguage，失败回退默认）；
 *   - 注册打开后焦点回归 effect（requestAnimationFrame 聚焦 textarea）；
 *   - 注册 Control 单键 / 组合快捷键 keydown+keyup 监听（capture 阶段，按 reducePromptOptimizerShortcut
 *     状态机判定，命中后触发 open/close/optimize）；
 *   - 暴露 `openPromptOptimizerPanel` / `closePromptOptimizerPanel` / `togglePromptOptimizerPanel` /
 *     `runPromptOptimization` / `handleCursorAnchorChange` / `handlePromptInputKeyDown` 稳定函数。
 *
 * 不复制邻接 controller 状态：project / session / worktree / terminal / file / automation 状态仍归
 * Workbench.tsx 或邻接 controller 所有。
 */
import { useCallback, useEffect, useRef, useState } from 'react';

import {
  canFillPromptIntoTerminal,
  createPromptOptimizerShortcutState,
  promptOptimizerInputKeyAction,
  promptOptimizerShortcutAction,
  reducePromptOptimizerShortcut,
  resetPromptOptimizerTextState,
  shouldCommitPromptOptimizerPanelPosition,
} from '../promptOptimizerWidget';
import type { PromptOptimizerShortcutState } from '../promptOptimizerWidget';
import type { TerminalCursorAnchor } from '../WorkbenchTerminalPane';
import type {
  PromptOptimizerFillLanguage,
  WorkbenchSession,
} from '@/lib/types';

/** 浮层相对 terminalArea 的定位（CSS custom property 来源）。 */
export interface PromptOptimizerPanelPosition {
  left: number;
  top: number;
}

/**
 * Prompt 优化浮层定位：把 viewport 坐标系的光标锚点转为 terminalArea 内相对坐标，
 * 并按面板宽高 clamp，避免浮层超出工作区。与原 Workbench.tsx 内联实现保持一致。
 */
export function promptOptimizerPanelPosition(
  areaRect: DOMRect,
  anchor: TerminalCursorAnchor,
): PromptOptimizerPanelPosition {
  const panelWidth = Math.min(560, Math.max(280, areaRect.width - 32));
  const estimatedPanelHeight = Math.min(520, Math.max(280, areaRect.height - 32));
  const maxLeft = Math.max(16, areaRect.width - panelWidth - 16);
  const maxTop = Math.max(16, areaRect.height - estimatedPanelHeight - 16);
  const left = Math.min(maxLeft, Math.max(16, anchor.left - areaRect.left));
  const top = Math.min(maxTop, Math.max(16, anchor.bottom - areaRect.top + 8));
  return { left, top };
}

/** controller 注入的配置加载函数（通常包装 configApi.get）。 */
export interface PromptOptimizerConfigLoadResult {
  promptOptimizerHotkey?: string | null;
  promptOptimizerFillLanguage?: PromptOptimizerFillLanguage | null;
}

/** 流式写入终端的注入函数（通常包装 promptOptimizerApi.streamToTerminal）。 */
export interface PromptOptimizerStreamResult {
  ok: boolean;
  sessionId: string;
}

export interface PromptOptimizerStreamToTerminalOptions {
  workingDirectory?: string | null;
  targetLanguage: PromptOptimizerFillLanguage;
  sessionId: string;
}

/**
 * controller 输入：窄 API + 回调 + 共享 refs，避免吞并 Projects / Terminal context。
 *
 * 字段说明：
 *   - activeSession / activeProjectId：从页面透传，仅用于读取（判定可否填入终端、错误上报目标 projectId）。
 *   - promptWorkingDirectory：由页面基于 active worktree 计算的绝对路径，传给 streamToTerminal。
 *   - remoteWriteDisabled：远端离线只读态，用于禁用写入与拦截快捷键打开。
 *   - automationConsoleOpen / workspaceView：跨域共享状态，用于判定快捷键是否应生效。
 *   - terminalAreaRef：terminalArea DOM ref，用于把光标锚点换算为浮层相对定位。
 *   - promptInputRef：prompt textarea DOM ref，用于打开后焦点回归与空输入时重新聚焦。
 *   - loadConfig：注入的配置加载（默认包装 configApi.get），便于单元测试替换。
 *   - streamToTerminal：注入的流式写入（默认包装 promptOptimizerApi.streamToTerminal）。
 *   - markRequestFailure：项目域 controller 的失败上报，用于 streamToTerminal reject 时记录离线。
 *   - setSessionError：终端域 session 错误回写（fill failed / optimize failed / 远端离线提示）。
 *   - displayErrorMessage：与终端域一致的错误文案构造器。
 *   - desktopUnavailableMessage / translateFillFailed / translateOptimizeFailed / translateRemoteOffline：
 *     i18n 文案注入（必须稳定引用，避免 controller 内 useCallback 依赖每次渲染都变）。
 */
export interface UseWorkbenchPromptOptimizerControllerParams {
  activeSession: WorkbenchSession | null;
  activeProjectId: string | null;
  promptWorkingDirectory: string | undefined;
  remoteWriteDisabled: boolean;
  automationConsoleOpen: boolean;
  workspaceView: 'terminal' | 'files' | 'browser';
  terminalAreaRef: React.RefObject<HTMLDivElement | null>;
  promptInputRef: React.RefObject<HTMLTextAreaElement | null>;
  loadConfig: () => Promise<PromptOptimizerConfigLoadResult>;
  streamToTerminal: (
    prompt: string,
    options: PromptOptimizerStreamToTerminalOptions,
  ) => Promise<PromptOptimizerStreamResult>;
  markRequestFailure: (projectId: string, error: unknown) => void;
  setSessionError: (message: string) => void;
  displayErrorMessage: (error: unknown, fallback: string, desktopUnavailable: string) => string;
  desktopUnavailableMessage: string;
  translateFillFailed: () => string;
  translateOptimizeFailed: () => string;
  translateRemoteOffline: () => string;
}

/**
 * controller 返回值：Prompt 优化浮层权威状态 + 操作函数。
 *
 * 字段语义：
 *   - promptPanelOpen / promptInput / promptOptimizing / promptOptimizerHotkey /
 *     promptOptimizerFillLanguage / promptPanelPosition：渲染浮层所需状态。
 *   - setPromptInput：textarea onChange 回写。
 *   - openPromptOptimizerPanel：清空输入并按光标锚点定位后打开浮层。
 *   - closePromptPanel：关闭浮层。
 *   - togglePromptOptimizerPanel：工具栏按钮切换（已开则关，已关则开）。
 *   - runPromptOptimization：校验后流式写入终端，成功关闭浮层。
 *   - handleCursorAnchorChange：终端光标移动回调，更新浮层定位。
 *   - handlePromptInputKeyDown：textarea onKeyDown，处理 Enter 提交（IME 安全）。
 */
export interface WorkbenchPromptOptimizerControllerResult {
  promptPanelOpen: boolean;
  promptInput: string;
  promptOptimizing: boolean;
  promptOptimizerHotkey: string;
  promptOptimizerFillLanguage: PromptOptimizerFillLanguage;
  promptPanelPosition: PromptOptimizerPanelPosition;
  setPromptInput: (next: string) => void;
  openPromptOptimizerPanel: () => void;
  closePromptPanel: () => void;
  togglePromptOptimizerPanel: () => void;
  runPromptOptimization: () => Promise<void>;
  handleCursorAnchorChange: (anchor: TerminalCursorAnchor | null) => void;
  handlePromptInputKeyDown: (event: React.KeyboardEvent<HTMLTextAreaElement>) => void;
}

/**
 * Business Logic（为什么是默认导出 hook）:
 *   Workbench.tsx 在 early return 之前调用本 hook，与其它 controller 并列组合；保持 React hooks 顺序稳定。
 *
 * Code Logic（这个 hook 做什么）:
 *   1. 持有浮层 state + 快捷键状态机 ref + 打开态 ref；
 *   2. 注册 config 加载 / 打开后焦点回归 / Control 快捷键监听三个 effect；
 *   3. 暴露稳定的操作函数（useCallback），由页面 handler / DOM 直接绑定。
 */
export function useWorkbenchPromptOptimizerController(
  params: UseWorkbenchPromptOptimizerControllerParams,
): WorkbenchPromptOptimizerControllerResult {
  const {
    activeSession,
    activeProjectId,
    promptWorkingDirectory,
    remoteWriteDisabled,
    automationConsoleOpen,
    workspaceView,
    terminalAreaRef,
    promptInputRef,
    loadConfig,
    streamToTerminal,
    markRequestFailure,
    setSessionError,
    displayErrorMessage,
    desktopUnavailableMessage,
    translateFillFailed,
    translateOptimizeFailed,
    translateRemoteOffline,
  } = params;

  const [promptPanelOpen, setPromptPanelOpen] = useState<boolean>(false);
  const [promptInput, setPromptInput] = useState<string>('');
  const [promptOptimizing, setPromptOptimizing] = useState<boolean>(false);
  const [promptOptimizerHotkey, setPromptOptimizerHotkey] = useState<string>('<ctrl>');
  const [promptOptimizerFillLanguage, setPromptOptimizerFillLanguage] =
    useState<PromptOptimizerFillLanguage>('zh');
  const [promptPanelPosition, setPromptPanelPosition] = useState<PromptOptimizerPanelPosition>({
    left: 24,
    top: 24,
  });

  // Business Logic: 快捷键状态机必须在 keydown/keyup 之间持续存在；用 ref 持有避免重渲染。
  const promptShortcutStateRef = useRef<PromptOptimizerShortcutState>(
    createPromptOptimizerShortcutState(),
  );
  // Business Logic: runPromptOptimization 与 handleCursorAnchorChange 在异步闭包里需要读取最新打开态；
  // 用 ref 镜像 promptPanelOpen，避免 useCallback 依赖 promptPanelOpen 每次渲染都变。
  const promptPanelOpenRef = useRef<boolean>(promptPanelOpen);
  useEffect(() => {
    promptPanelOpenRef.current = promptPanelOpen;
  }, [promptPanelOpen]);

  // Business Logic: 终端光标锚点 ref 必须跨渲染持续存在，且能被 handleCursorAnchorChange 与
  // openPromptOptimizerPanel 共同读写；声明在 hooks 区域（early return 之前）。
  const cursorAnchorRef = useRef<TerminalCursorAnchor | null>(null);

  // Business Logic: mount 时拉取用户配置的快捷键与单语设置；普通浏览器调试环境 configApi 会 reject，
  // 此时保留默认快捷键 '<ctrl>' 与中文设置（与原 Workbench.tsx 行为一致）。
  useEffect(() => {
    let cancelled = false;
    void loadConfig()
      .then((config) => {
        if (cancelled) return;
        setPromptOptimizerHotkey(config.promptOptimizerHotkey || '<ctrl>');
        setPromptOptimizerFillLanguage(
          config.promptOptimizerFillLanguage === 'en' ? 'en' : 'zh',
        );
      })
      .catch(() => {
        // 普通浏览器调试环境没有 Tauri invoke；保留默认快捷键与语言即可。
      });
    return () => {
      cancelled = true;
    };
  }, [loadConfig]);

  // Business Logic: 浮层打开后应把焦点放到 textarea，让用户立刻输入；用 rAF 等待 DOM 渲染完成。
  useEffect(() => {
    if (!promptPanelOpen) return undefined;
    const frame = window.requestAnimationFrame(() => {
      promptInputRef.current?.focus();
    });
    return () => window.cancelAnimationFrame(frame);
  }, [promptPanelOpen, promptInputRef]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   重新打开浮层时必须清空上一次输入，并把浮层定位到当前终端光标下方。
   *
   * Code Logic（这个函数做什么）:
   *   复用 resetPromptOptimizerTextState 清空输入；若 terminalArea 与光标锚点都存在，
   *   用 promptOptimizerPanelPosition 换算相对定位后写入 state；最后置 panelOpen=true。
   */
  const openPromptOptimizerPanel = useCallback(() => {
    const reset = resetPromptOptimizerTextState();
    setPromptInput(reset.input);
    const area = terminalAreaRef.current;
    // Business Logic: 浮层定位依赖终端光标锚点；锚点 ref 由页面通过 handleCursorAnchorChange 持续更新。
    // 这里读取挂在 controller 实例上的 cursorAnchorRef（见下方）。
    const anchor = cursorAnchorRef.current;
    if (area && anchor) {
      setPromptPanelPosition(promptOptimizerPanelPosition(area.getBoundingClientRect(), anchor));
    }
    setPromptPanelOpen(true);
  }, [terminalAreaRef]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   关闭浮层是工具栏按钮、Control 快捷键、优化成功的共同出口。
   *
   * Code Logic（这个函数做什么）:
   *   置 panelOpen=false。不清空输入（重新打开时由 openPromptOptimizerPanel 清空），
   *   保持与原 Workbench.tsx 行为一致。
   */
  const closePromptPanel = useCallback(() => {
    setPromptPanelOpen(false);
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   校验并执行流式写入；成功关闭浮层，失败按远端离线 / 通用错误分别上报。
   *
   * Code Logic（这个函数做什么）:
   *   - 空输入或正在优化时重新聚焦 textarea 并返回（不提交空任务）；
   *   - 无活动 session 或活动 session 非 running 时设置 fill failed 错误并返回；
   *   - 远端离线时设置远端离线提示并返回；
   *   - 调用注入的 streamToTerminal，成功后关闭浮层；失败时上报 markRequestFailure +
   *     setSessionError(displayErrorMessage(...))；finally 恢复 optimizing=false。
   */
  const runPromptOptimization = useCallback(async () => {
    if (!promptInput.trim() || promptOptimizing) {
      promptInputRef.current?.focus();
      return;
    }
    if (!activeSession || !canFillPromptIntoTerminal(activeSession)) {
      setSessionError(translateFillFailed());
      return;
    }
    if (remoteWriteDisabled) {
      setSessionError(translateRemoteOffline());
      return;
    }
    try {
      setPromptOptimizing(true);
      await streamToTerminal(promptInput, {
        workingDirectory: promptWorkingDirectory,
        targetLanguage: promptOptimizerFillLanguage,
        sessionId: activeSession.id,
      });
      setPromptPanelOpen(false);
    } catch (error) {
      if (activeProjectId) {
        markRequestFailure(activeProjectId, error);
      }
      setSessionError(
        displayErrorMessage(error, translateOptimizeFailed(), desktopUnavailableMessage),
      );
    } finally {
      setPromptOptimizing(false);
    }
  }, [
    activeSession,
    activeProjectId,
    desktopUnavailableMessage,
    displayErrorMessage,
    markRequestFailure,
    promptInput,
    promptInputRef,
    promptOptimizing,
    promptOptimizerFillLanguage,
    promptWorkingDirectory,
    remoteWriteDisabled,
    setSessionError,
    streamToTerminal,
    translateFillFailed,
    translateOptimizeFailed,
    translateRemoteOffline,
  ]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   Control 单键 / 组合快捷键命中后需要根据当前打开态与输入内容决定 open/close/optimize。
   *
   * Code Logic（这个函数做什么）:
   *   - 自动化控制台打开或不在终端视图时不响应；
   *   - 远端离线且浮层未打开时不响应（避免在只读态唤起必然失败的浮层）；
   *   - 复用 promptOptimizerShortcutAction 判定 open/close/optimize 并分派。
   */
  const triggerPromptOptimizerShortcut = useCallback(() => {
    if (automationConsoleOpen) return;
    if (!activeProjectId || workspaceView !== 'terminal') return;
    if (remoteWriteDisabled && !promptPanelOpenRef.current) return;
    const action = promptOptimizerShortcutAction(promptPanelOpenRef.current, promptInput);
    if (action === 'open') {
      openPromptOptimizerPanel();
      return;
    }
    if (action === 'close') {
      setPromptPanelOpen(false);
      return;
    }
    void runPromptOptimization();
  }, [
    activeProjectId,
    automationConsoleOpen,
    openPromptOptimizerPanel,
    promptInput,
    runPromptOptimization,
    remoteWriteDisabled,
    workspaceView,
  ]);

  // Business Logic: Control 单键快捷键需要在 keydown+keyup 两阶段判定（避免 Ctrl+C 误触发）；
  // 在 capture 阶段监听 window，让终端其它 keydown 监听不会先消费掉 Control。
  useEffect(() => {
    const handleShortcutEvent = (event: KeyboardEvent) => {
      if (automationConsoleOpen) return;
      if (workspaceView !== 'terminal') return;
      const result = reducePromptOptimizerShortcut(
        promptShortcutStateRef.current,
        {
          type: event.type === 'keyup' ? 'keyup' : 'keydown',
          key: event.key,
          ctrlKey: event.ctrlKey,
          metaKey: event.metaKey,
          altKey: event.altKey,
          shiftKey: event.shiftKey,
          repeat: event.repeat,
        },
        promptOptimizerHotkey,
      );
      promptShortcutStateRef.current = result.state;
      if (!result.triggered) return;
      event.preventDefault();
      event.stopPropagation();
      triggerPromptOptimizerShortcut();
    };

    window.addEventListener('keydown', handleShortcutEvent, { capture: true });
    window.addEventListener('keyup', handleShortcutEvent, { capture: true });
    return () => {
      window.removeEventListener('keydown', handleShortcutEvent, { capture: true });
      window.removeEventListener('keyup', handleShortcutEvent, { capture: true });
    };
  }, [automationConsoleOpen, promptOptimizerHotkey, triggerPromptOptimizerShortcut, workspaceView]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   浮层定位跟随终端光标；光标高频移动，浮层关闭时不应强制布局读取或触发重渲染。
   *
   * Code Logic（这个函数做什么）:
   *   始终更新 cursorAnchorRef；仅当浮层打开且 anchor 非空时才读 terminalArea.getBoundingClientRect 并
   *   在 left/top 变化时提交新定位（shouldCommitPromptOptimizerPanelPosition gate）。
   */
  const handleCursorAnchorChange = useCallback(
    (anchor: TerminalCursorAnchor | null) => {
      cursorAnchorRef.current = anchor;
      if (!promptPanelOpenRef.current || !anchor) return;
      const area = terminalAreaRef.current;
      if (!area) return;
      const nextPosition = promptOptimizerPanelPosition(area.getBoundingClientRect(), anchor);
      setPromptPanelPosition((current) =>
        shouldCommitPromptOptimizerPanelPosition(true, current, nextPosition)
          ? nextPosition
          : current,
      );
    },
    [terminalAreaRef],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   textarea 的 Enter 提交必须避开 IME 选词与 Shift+Enter 换行。
   *
   * Code Logic（这个函数做什么）:
   *   复用 promptOptimizerInputKeyAction 判定；optimize 时阻止默认行为并触发 runPromptOptimization。
   */
  const handlePromptInputKeyDown = useCallback(
    (event: React.KeyboardEvent<HTMLTextAreaElement>) => {
      const action = promptOptimizerInputKeyAction(
        {
          key: event.key,
          shiftKey: event.shiftKey,
          isComposing: event.nativeEvent.isComposing,
        },
        promptInput,
      );
      if (action !== 'optimize') return;
      event.preventDefault();
      event.stopPropagation();
      void runPromptOptimization();
    },
    [promptInput, runPromptOptimization],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   工具栏“Prompt 优化”按钮是 toggle：已开则关，已关则开。
   *
   * Code Logic（这个函数做什么）:
   *   读取 promptPanelOpenRef 当前值分派 open/close（不用依赖 promptPanelOpen state，保持稳定引用）。
   */
  const togglePromptOptimizerPanel = useCallback(() => {
    if (promptPanelOpenRef.current) {
      setPromptPanelOpen(false);
    } else {
      openPromptOptimizerPanel();
    }
  }, [openPromptOptimizerPanel]);

  return {
    promptPanelOpen,
    promptInput,
    promptOptimizing,
    promptOptimizerHotkey,
    promptOptimizerFillLanguage,
    promptPanelPosition,
    setPromptInput,
    openPromptOptimizerPanel,
    closePromptPanel,
    togglePromptOptimizerPanel,
    runPromptOptimization,
    handleCursorAnchorChange,
    handlePromptInputKeyDown,
  };
}
