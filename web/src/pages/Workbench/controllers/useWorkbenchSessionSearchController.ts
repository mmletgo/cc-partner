/**
 * Workbench Session 搜索浮层 controller —— ⌘K/Ctrl+K 开/关、工具栏入口与 resume 成功编排。
 *
 * Business Logic（为什么需要这个 controller）:
 *   用户在 Workbench 终端里需要快速找回历史 Claude 会话；按 ⌘K（mac）/ Ctrl+K（其它平台）唤起
 *   Command Palette 浮层，在当前 worktree 范围搜索、预览并 resume 历史 session。浮层只在终端视图
 *   可用（文件/浏览器视图下快捷键不响应），resume 成功后需要刷新当前项目的 sessions 列表并把焦点
 *   切到新 window（终端域 focusSession），随后关闭浮层。这些开/关与 resume 编排逻辑原本散落在
 *   Workbench.tsx 的 state + keydown effect + onResumed 内联回调里，本 controller 把它们集中持有。
 *
 *   重要边界（与其它 controller 一致）：
 *   - `workspaceView` 是跨域共享状态（终端全屏、自动化控制台、文件 tab 都会改写），仍归 Workbench.tsx
 *     所有；controller 只读取它来判断快捷键是否应生效。
 *   - 搜索结果数据（query / hits / preview / debounce 等）仍归 `WorkbenchSessionSearch` 组件所有，
 *     controller 绝不复制或代理这些状态。
 *   - controller 不持有 session 列表、worktree 列表或终端字节内容；这些仍归邻接 controller / 页面。
 *
 * Code Logic（这个 controller 做什么）:
 *   - 持有 `sessionSearchOpen` state；
 *   - 注册 ⌘K/Ctrl+K keydown 监听（仅终端视图；modifier 组合与 repeat 守卫与原 Workbench.tsx 一致）；
 *   - 暴露 `openSessionSearch` / `closeSessionSearch` / `handleResumed` 稳定函数。
 *
 * 不复制邻接 controller 状态：project / session / worktree / terminal / file / automation 状态仍归
 * Workbench.tsx 或邻接 controller 所有。
 */
import { useCallback, useEffect, useState } from 'react';

/**
 * controller 输入：窄 API + 回调，避免吞并 Terminal context。
 *
 * 字段说明：
 *   - workspaceView：跨域共享状态，用于判定快捷键是否应生效（仅终端视图）。
 *   - activeProjectId：当前活动项目 id；resume 成功后用它触发 loadSessions 刷新（缺失时跳过刷新）。
 *   - loadSessions：终端域 controller 的 sessions 加载 API；resume 成功后调用以拉取新 window。
 *   - focusSession：终端域 controller 的 session focus API；resume 成功后把焦点切到新 session。
 */
export interface UseWorkbenchSessionSearchControllerParams {
  workspaceView: 'terminal' | 'files' | 'browser';
  activeProjectId: string | null;
  loadSessions: (projectId: string) => Promise<void>;
  focusSession: (sessionId: string) => Promise<boolean>;
}

/**
 * controller 返回值：Session 搜索浮层开/关权威状态 + 操作函数。
 *
 * 字段语义：
 *   - sessionSearchOpen：浮层是否展开（驱动 WorkbenchSessionSearch 的 open prop）。
 *   - openSessionSearch：展开浮层（工具栏按钮 + ⌘K 快捷键共同入口）。
 *   - closeSessionSearch：收起浮层（WorkbenchSessionSearch 的 onClose 回调）。
 *   - handleResumed：resume 成功回调；刷新 sessions + focus 新 session + 关闭浮层。
 */
export interface WorkbenchSessionSearchControllerResult {
  sessionSearchOpen: boolean;
  openSessionSearch: () => void;
  closeSessionSearch: () => void;
  handleResumed: (newSessionId: string) => Promise<void>;
}

/**
 * Business Logic（为什么是默认导出 hook）:
 *   Workbench.tsx 在 early return 之前调用本 hook，与其它 controller 并列组合；保持 React hooks 顺序稳定。
 *
 * Code Logic（这个 hook 做什么）:
 *   1. 持有 sessionSearchOpen state；
 *   2. 注册 ⌘K/Ctrl+K keydown 监听（workspaceView==='terminal' 时挂载，否则卸载）；
 *   3. 暴露稳定的 open/close/handleResumed 函数（useCallback），由页面 handler / 组件回调绑定。
 */
export function useWorkbenchSessionSearchController(
  params: UseWorkbenchSessionSearchControllerParams,
): WorkbenchSessionSearchControllerResult {
  const { workspaceView, activeProjectId, loadSessions, focusSession } = params;

  const [sessionSearchOpen, setSessionSearchOpen] = useState<boolean>(false);

  // Business Logic: ⌘K / Ctrl+K 打开 Claude session 搜索浮层，仅终端视图生效。
  // Code Logic: 与原 Workbench.tsx 行为一致——仅 keydown、忽略 repeat、要求 meta 或 ctrl 之一且不伴随
  // alt/shift（避免与 Cmd+Shift+K 等其它全局快捷键冲突），命中后 preventDefault 并打开浮层。
  // 监听器随 workspaceView 变化挂载/卸载，离开终端视图时立即移除。
  useEffect(() => {
    if (workspaceView !== 'terminal') return undefined;
    const handleSessionSearchShortcut = (event: KeyboardEvent) => {
      if (event.repeat) return;
      if ((event.metaKey || event.ctrlKey) && !event.altKey && !event.shiftKey && event.key === 'k') {
        event.preventDefault();
        setSessionSearchOpen(true);
      }
    };
    window.addEventListener('keydown', handleSessionSearchShortcut);
    return () => window.removeEventListener('keydown', handleSessionSearchShortcut);
  }, [workspaceView]);

  const openSessionSearch = useCallback(() => {
    setSessionSearchOpen(true);
  }, []);

  const closeSessionSearch = useCallback(() => {
    setSessionSearchOpen(false);
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   resume 成功后新 window 已创建，必须刷新当前项目的 sessions 列表（让侧栏/终端 tab 反映新 session）
   *   并把焦点切到新 session（终端域 focusSession 会触发后端 focus 命令），最后关闭浮层。
   *
   * Code Logic（这个函数做什么）:
   *   有 activeProjectId 时触发 loadSessions（不 await 阻塞焦点切换，与原 Workbench.tsx 行为一致）；
   *   始终调用 focusSession(newSessionId)；最后关闭浮层。
   */
  const handleResumed = useCallback(
    async (newSessionId: string) => {
      if (activeProjectId) {
        void loadSessions(activeProjectId);
      }
      focusSession(newSessionId);
      setSessionSearchOpen(false);
    },
    [activeProjectId, focusSession, loadSessions],
  );

  return {
    sessionSearchOpen,
    openSessionSearch,
    closeSessionSearch,
    handleResumed,
  };
}
