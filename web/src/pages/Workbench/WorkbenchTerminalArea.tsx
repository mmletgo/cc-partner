/**
 * Workbench 终端区域叶子视图 —— Prompt 优化浮层 + 终端 pane 集合。
 *
 * Business Logic（为什么需要这个组件）:
 *   Plan 2 Task 8 把 Workbench.tsx centerPane 内 terminalArea 的 JSX 抽到独立叶子组件，便于页面降到 ≤1200 行。
 *   组件只接收 controller 派生的渲染数据与回调，不持有自己的状态；refs 由页面持有并通过 props 注入。
 *
 * Code Logic（这个组件做什么）:
 *   - 渲染 Prompt 优化浮层（条件：promptPanelOpen && !terminalFullscreen）；
 *   - 渲染空态 TerminalPane 或 mountedSessions 对应的 TerminalPane 列表；
 *   - 暴露 WorkbenchTerminalAreaProps 类型，所有数据均来自 useWorkbenchTerminalController /
 *     useWorkbenchPromptOptimizerController + Workbench.tsx 跨域共享。
 */
import type { CSSProperties, RefObject } from 'react';
import { useTranslation } from 'react-i18next';
import type { WorkbenchSession } from '@/lib/types';
import styles from './Workbench.module.css';
import { WorkbenchTerminalPane } from './WorkbenchTerminalPane';
import type { TerminalCursorAnchor } from './WorkbenchTerminalPane';

/**
 * 终端区域叶子组件的输入 props。
 *
 * Business Logic: 所有数据均由 useWorkbenchTerminalController + useWorkbenchPromptOptimizerController +
 * Workbench.tsx 跨域共享派生；组件本身不持有状态、不调用 workbenchApi。
 * terminalAreaRef / terminalPanelRef 由页面持有并透传，因为控制器需要它们做终端 sizing。
 */
export interface WorkbenchTerminalAreaProps {
  terminalAreaRef: RefObject<HTMLDivElement | null>;
  terminalPanelRef: RefObject<HTMLElement | null>;
  promptPanelOpen: boolean;
  terminalFullscreen: boolean;
  promptPanelStyle: CSSProperties;
  promptInputRef: RefObject<HTMLTextAreaElement | null>;
  promptInput: string;
  setPromptInput: (next: string) => void;
  handlePromptInputKeyDown: (
    event: React.KeyboardEvent<HTMLTextAreaElement>,
  ) => void;
  promptOptimizing: boolean;
  remoteWriteDisabled: boolean;
  automationConsoleOpen: boolean;
  workspaceView: string;
  activeProject: { name?: string } | null;
  visibleSessions: WorkbenchSession[];
  mountedSessions: WorkbenchSession[];
  renderedActiveSessionId: string | null;
  terminalResizeRequestKey: number;
  handleInput: (sessionId: string, data: string) => void;
  /**
   * Business Logic（为什么需要这个回调）:
   *   write 失败后输入泵 silent-block enqueue；UI 必须禁用 xterm 输入，避免键盘黑洞。
   *   非 running session 也不得接受输入，即使 write-block 已解除或尚未封锁。
   *
   * Code Logic（这个回调做什么）:
   *   查询 controller 的 isWriteBlocked(sessionId)；true 时 inputEnabled=false。
   *   最终 inputEnabled 还须要求 session.status === 'running'。
   */
  isWriteBlocked: (sessionId: string) => boolean;
  handleResize: (sessionId: string, cols: number, rows: number) => void;
  handleCursorAnchorChange: (anchor: TerminalCursorAnchor | null) => void;
  /**
   * Business Logic（为什么需要这个回调）:
   *   多 pane 终端的字符格点击由 controller 委托给后端做 tmux 真实布局命中。
   *   已离屏 / 自动化 / browser/files 视图下禁用，避免把点击事件误传到非 pane 视图。
   */
  handleSelectPaneAt: (sessionId: string, col: number, row: number) => void;
  focusSession: (sessionId: string) => void;
}

/**
 * Business Logic（为什么需要这个组件）:
 *   Workbench centerPane 的 terminalArea 需要稳定渲染 Prompt 浮层 + 终端 pane 集合，让用户在多个 session 间切换。
 *
 * Code Logic（这个组件做什么）:
 *   渲染 Prompt 浮层（条件渲染）+ 终端 pane（空态占位或多 pane 列表）；不持有状态、不调用 workbenchApi。
 */
export function WorkbenchTerminalArea(props: WorkbenchTerminalAreaProps) {
  const { t } = useTranslation(['workbench', 'promptOptimizer']);
  const {
    terminalAreaRef,
    terminalPanelRef,
    promptPanelOpen,
    terminalFullscreen,
    promptPanelStyle,
    promptInputRef,
    promptInput,
    setPromptInput,
    handlePromptInputKeyDown,
    promptOptimizing,
    remoteWriteDisabled,
    automationConsoleOpen,
    workspaceView,
    activeProject,
    visibleSessions,
    mountedSessions,
    renderedActiveSessionId,
    terminalResizeRequestKey,
    handleInput,
    isWriteBlocked,
    handleResize,
    handleCursorAnchorChange,
    handleSelectPaneAt,
    focusSession,
  } = props;

  const cursorAnchorActive =
    !automationConsoleOpen && workspaceView === 'terminal' ? handleCursorAnchorChange : undefined;
  // Business Logic: fullscreen 终端会覆盖其他 workspace；非 fullscreen 时只有 terminal 视图的 active pane 真正可见。
  const terminalSurfaceVisible =
    terminalFullscreen || (!automationConsoleOpen && workspaceView === 'terminal');

  return (
    <div className={styles.terminalArea} ref={terminalAreaRef}>
      {promptPanelOpen && !terminalFullscreen ? (
        <aside
          className={styles.promptOptimizerPanel}
          style={promptPanelStyle}
          aria-label={t('workbench:promptOptimizer.panelAriaLabel')}
        >
          <textarea
            ref={promptInputRef}
            className={styles.promptOptimizerInput}
            value={promptInput}
            onChange={(event) => setPromptInput(event.target.value)}
            onKeyDown={handlePromptInputKeyDown}
            placeholder={t('promptOptimizer:inputPlaceholder')}
            aria-label={t('promptOptimizer:inputAriaLabel')}
            disabled={promptOptimizing || remoteWriteDisabled}
          />
        </aside>
      ) : null}

      <section
        className={styles.terminalPanel}
        data-layout="single"
        ref={terminalPanelRef}
      >
        {visibleSessions.length === 0 ? (
          <WorkbenchTerminalPane
            session={null}
            placeholder={
              activeProject
                ? t('workbench:terminalPlaceholder')
                : t('workbench:terminalNoProject')
            }
            onInput={handleInput}
            onResize={handleResize}
            resizeRequestKey={0}
            renderVisible={terminalSurfaceVisible}
            inputEnabled={false}
            onCursorAnchorChange={cursorAnchorActive}
          />
        ) : null}
        {mountedSessions.map((session) => (
          <div
            key={session.id}
            className={styles.terminalPaneFrame}
            data-active={session.id === renderedActiveSessionId || undefined}
            onClick={() => {
              // 已聚焦 window 上的文本选中/复制不得再触发 focusSession；
              // 否则 remote focus 路径会 resync 并立刻清掉 xterm 选区。
              if (session.id !== renderedActiveSessionId) {
                void focusSession(session.id);
              }
            }}
          >
            <div className={styles.terminalPaneHeader}>
              <span className={styles.sessionDot} data-status={session.status} />
              <span className={styles.sessionName}>{session.name}</span>
              <span className={styles.terminalPaneStatus}>
                {session.status === 'running'
                  ? t('workbench:sessionStatus.running')
                  : session.status === 'exited'
                    ? t('workbench:sessionStatus.exited')
                    : session.status === 'disconnected'
                      ? t('workbench:sessionStatus.disconnected')
                      : session.status}
              </span>
            </div>
            <WorkbenchTerminalPane
              session={session}
              placeholder={t('workbench:terminalPlaceholder')}
              onInput={handleInput}
              onResize={handleResize}
              resizeRequestKey={
                session.id === renderedActiveSessionId ? terminalResizeRequestKey : 0
              }
              renderVisible={
                terminalSurfaceVisible && session.id === renderedActiveSessionId
              }
              inputEnabled={
                !automationConsoleOpen &&
                workspaceView === 'terminal' &&
                session.id === renderedActiveSessionId &&
                !remoteWriteDisabled &&
                session.status === 'running' &&
                !isWriteBlocked(session.id)
              }
              onCursorAnchorChange={
                !automationConsoleOpen &&
                workspaceView === 'terminal' &&
                session.id === renderedActiveSessionId
                  ? handleCursorAnchorChange
                  : undefined
              }
              onSelectPaneAt={
                !automationConsoleOpen &&
                workspaceView === 'terminal' &&
                session.id === renderedActiveSessionId
                  ? handleSelectPaneAt
                  : undefined
              }
            />
          </div>
        ))}
      </section>
    </div>
  );
}
