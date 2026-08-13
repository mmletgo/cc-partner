/**
 * Workbench「收藏快捷输入」叶子 hook
 *
 * Business Logic（为什么需要这个 hook）:
 *   用户在终端区域工作时，希望用快捷键或工具栏按钮一键唤出收藏的 Prompt 列表，
 *   选中后把内容插入当前会话输入行（不回车），减少在 Prompt 库与终端之间切换的摩擦。
 *   该能力独立于 7 个 Workbench 聚合 controller（CLAUDE.md 禁止第 8 个 controller），
 *   作为叶子 hook 存在，参考 useAgentRuntime 的先例。
 *
 * Code Logic（这个 hook 做什么）:
 *   - mount 时经 configApi.get 读取 promptQuickInputHotkey（失败回退默认 <ctrl>+/"）。
 *   - window capture keydown 监听：命中快捷键且焦点在终端区域时 preventDefault + toggle 浮层
 *     （纯 Tab 等单键配置下 preventDefault 拦截键入 shell）。
 *   - 持有 open/selectedTag/query/收藏列表 state；浮层打开时拉取 promptsApi.list 并客户端过滤 favorite。
 *   - 选中 prompt 调注入的 handleInput(sessionId, content)（不拼 \r，只插入不回车）后关闭浮层。
 *   - 所有 hooks 位于 early return 之前；本 hook 不包含 early return。
 */

import { useCallback, useEffect, useRef, useState } from 'react';
import type { RefObject } from 'react';
import { configApi } from '@/api/config';
import { promptsApi } from '@/api/prompts';
import type { Prompt } from '@/lib/types';
import {
  closeFavoriteQuickInputPanel,
  createFavoriteQuickInputState,
  FAVORITE_QUICK_INPUT_DEFAULT_HOTKEY,
  isFavoriteQuickInputShortcut,
  openFavoriteQuickInputPanel,
  setFavoriteQuickInputQuery,
  setFavoriteQuickInputTag,
} from '../favoriteQuickInputWidget';

/** 注入终端输入的回调签名（与 useWorkbenchTerminalController.handleInput 一致）。 */
export interface UseWorkbenchFavoriteQuickInputParams {
  /** 当前活跃终端会话 id；为 null 时选中 prompt 不会注入 */
  activeSessionId: string | null;
  /** 终端面板包装 ref，用于焦点判定（指向 .terminalPanel section） */
  terminalPanelRef: RefObject<HTMLElement | null>;
  /** 终端输入写入回调（经输入泵 enqueueInput，不拼 \r） */
  handleInput: (sessionId: string, data: string) => void;
}

/** 叶子 hook 返回值：浮层状态、收藏数据与用户动作回调。 */
export interface UseWorkbenchFavoriteQuickInputResult {
  open: boolean;
  selectedTag: string;
  query: string;
  /** 当前生效的快捷键（用于 UI 展示或调试，浮层本身不直接展示） */
  hotkey: string;
  favoritePrompts: Prompt[];
  loading: boolean;
  loadError: string | null;
  onToggle: () => void;
  onSelectTag: (tag: string) => void;
  onQueryChange: (query: string) => void;
  onSelectPrompt: (prompt: Prompt) => void;
  onClose: () => void;
}

/**
 * Business Logic（为什么需要这个焦点判定）:
 *   快捷键监听挂在 window，若不限定焦点区域，用户在搜索框、文件树或代码编辑器输入时
 *   也会触发浮层，干扰其他视图。只有焦点在终端区域（xterm 容器或其包装 section）才响应。
 *
 * Code Logic（这个函数做什么）:
 *   terminalPanelRef.current.contains(activeElement) 或 activeElement.closest('.xterm') 命中即视为焦点在终端。
 */
function isFocusInTerminal(
  terminalPanelRef: RefObject<HTMLElement | null>,
  active: Element | null,
): boolean {
  if (!active) return false;
  const panel = terminalPanelRef.current;
  if (panel && panel.contains(active)) return true;
  return active.closest('.xterm') !== null;
}

/**
 * 收藏快捷输入叶子 hook。
 *
 * Business Logic（为什么需要这个函数）:
 *   Workbench.tsx 在 early return 前调用本 hook 接线浮层与工具栏按钮；hook 自包含快捷键
 *   读取、焦点判定与收藏列表加载，调用方只需注入 activeSessionId/terminalPanelRef/handleInput。
 *
 * Code Logic（这个函数做什么）:
 *   持有 state，注册 mount 期 hotkey 读取 effect、open 期收藏列表加载 effect、capture keydown effect；
 *   返回受控状态与回调。所有 hooks 无条件执行，无 early return。
 */
export function useWorkbenchFavoriteQuickInput(
  params: UseWorkbenchFavoriteQuickInputParams,
): UseWorkbenchFavoriteQuickInputResult {
  const { activeSessionId, terminalPanelRef, handleInput } = params;

  const [state, setState] = useState(createFavoriteQuickInputState);
  const [hotkey, setHotkey] = useState<string>(FAVORITE_QUICK_INPUT_DEFAULT_HOTKEY);
  const [favoritePrompts, setFavoritePrompts] = useState<Prompt[]>([]);
  const [loading, setLoading] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const openRef = useRef(false);
  const loadSeqRef = useRef(0);

  // mount 时读配置的快捷键，失败回退默认
  useEffect(() => {
    let cancelled = false;
    const loadHotkey = async () => {
      try {
        const config = await configApi.get();
        if (cancelled) return;
        setHotkey(config.promptQuickInputHotkey || FAVORITE_QUICK_INPUT_DEFAULT_HOTKEY);
      } catch {
        // 配置读取失败时保持默认快捷键，不阻塞浮层功能
      }
    };
    void loadHotkey();
    return () => {
      cancelled = true;
    };
  }, []);

  const loadFavorites = useCallback(async () => {
    const seq = ++loadSeqRef.current;
    setLoading(true);
    setLoadError(null);
    try {
      const list = await promptsApi.list();
      if (seq !== loadSeqRef.current) return;
      const safe = Array.isArray(list) ? list : [];
      setFavoritePrompts(safe.filter((p) => p.favorite));
    } catch (err) {
      if (seq !== loadSeqRef.current) return;
      setLoadError(err instanceof Error ? err.message : '');
    } finally {
      if (seq === loadSeqRef.current) {
        setLoading(false);
      }
    }
  }, []);

  /** 工具栏与快捷键共用的开关；仅 closed → open 时刷新收藏。 */
  const togglePanel = useCallback(() => {
    const nextOpen = !openRef.current;
    openRef.current = nextOpen;
    setState((previous) =>
      nextOpen
        ? openFavoriteQuickInputPanel(previous)
        : closeFavoriteQuickInputPanel(previous),
    );
    if (nextOpen) {
      void loadFavorites();
    }
  }, [loadFavorites]);

  // window capture keydown：命中快捷键且焦点在终端时 preventDefault + toggle
  useEffect(() => {
    if (!hotkey) return undefined;
    const handler = (event: KeyboardEvent) => {
      if (event.defaultPrevented) return;
      if (!isFavoriteQuickInputShortcut(event, hotkey)) return;
      if (!isFocusInTerminal(terminalPanelRef, document.activeElement)) return;
      // 纯 Tab 等单键配置下必须 preventDefault，避免 Tab 送入 shell；组合键也 preventDefault 保持一致
      event.preventDefault();
      togglePanel();
    };
    window.addEventListener('keydown', handler, true);
    return () => window.removeEventListener('keydown', handler, true);
  }, [hotkey, terminalPanelRef, togglePanel]);

  const onToggle = useCallback(() => {
    togglePanel();
  }, [togglePanel]);

  const onSelectTag = useCallback((tag: string) => {
    setState((prev) => setFavoriteQuickInputTag(prev, tag));
  }, []);

  const onQueryChange = useCallback((query: string) => {
    setState((prev) => setFavoriteQuickInputQuery(prev, query));
  }, []);

  const onSelectPrompt = useCallback(
    (prompt: Prompt) => {
      if (activeSessionId) {
        // 只插入内容，不拼 \r，让用户在终端输入行继续编辑后自行回车
        handleInput(activeSessionId, prompt.content);
      }
      openRef.current = false;
      setState((prev) => closeFavoriteQuickInputPanel(prev));
    },
    [activeSessionId, handleInput],
  );

  const onClose = useCallback(() => {
    openRef.current = false;
    setState((prev) => closeFavoriteQuickInputPanel(prev));
  }, []);

  return {
    open: state.open,
    selectedTag: state.selectedTag,
    query: state.query,
    hotkey,
    favoritePrompts,
    loading,
    loadError,
    onToggle,
    onSelectTag,
    onQueryChange,
    onSelectPrompt,
    onClose,
  };
}
