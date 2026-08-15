/**
 * Workbench 终端 window tablist。
 *
 * Business Logic（为什么需要这个组件）:
 *   终端 window tabs 需要独立的 roving 键盘语义与 close 后焦点回落，
 *   抽离后避免继续膨胀 Workbench.tsx，并与文件 tabs 保持同源 DOM 合同。
 *
 * Code Logic（这个组件做什么）:
 *   渲染 role=tablist：每个 session 为 tab button + sibling close；选中 tabIndex=0；
 *   Arrow/Home/End 经 getRovingTabIndex 激活；关闭选中项后 focus 相邻或新建按钮。
 */

import { useCallback, useEffect, useRef, useState } from 'react';
import type { KeyboardEvent, MouseEvent, ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, HintStatusDot } from '@/components/primitives';
import { useOptionalWorkbenchAgentHints } from '@/hooks/workbenchAgentHintsContext';
import { EMPTY_HINT_COUNTS, hintCountsFrom, type AgentHintCounts } from '@/lib/workbenchAgentHints';
import { agentHintAriaSpec } from './workbenchAgentHintPresentation';
import { PlusIcon, XIcon } from '@/lib/icons';
import { getRovingTabIndex, type RovingTabKey } from '@/lib/rovingTablist';
import type { WorkbenchSession } from '@/lib/types';
import styles from './Workbench.module.css';

const NEW_SESSION_BUTTON_ID = 'workbench-session-tab-new';

/**
 * Business Logic（为什么需要这个函数）:
 *   终端 session tab 键盘 roving 与关闭后焦点回落都依赖稳定 button id。
 *
 * Code Logic（这个函数做什么）:
 *   用 sessionId 拼接 DOM id。
 */
function sessionTabButtonId(sessionId: string): string {
  return `workbench-session-tab-${sessionId}`;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   关闭/方向键切换后焦点不能丢到 body。
 *
 * Code Logic（这个函数做什么）:
 *   下一帧起最多重试 8 次按 id focus。
 */
function focusElementById(elementId: string): void {
  if (typeof window === 'undefined') return;
  const tryFocus = (attempt: number): void => {
    const node = document.getElementById(elementId);
    if (node) {
      node.focus();
      return;
    }
    if (attempt >= 8) return;
    window.requestAnimationFrame(() => tryFocus(attempt + 1));
  };
  window.requestAnimationFrame(() => tryFocus(0));
}

/**
 * WorkbenchSessionTabs 输入 props。
 *
 * Business Logic（为什么需要这个类型）:
 *   页面只透传 session 列表与 create/close/focus 动作，不把键盘细节留在 Workbench.tsx。
 */
export interface WorkbenchSessionTabsProps {
  sessions: WorkbenchSession[];
  activeSessionId: string | null;
  sessionBusy: boolean;
  canCreate: boolean;
  onFocusSession: (sessionId: string) => void;
  onCloseSession: (sessionId: string) => Promise<void>;
  onCreateSession: () => void;
  /**
   * 双击 tab 名行内重命名（复用既有 rename_workbench_session 链路）；返回是否成功。
   * 仅桌面终端 tabs 使用；远端离线时页面传 canRename=false 禁用。
   */
  onRenameSession: (sessionId: string, name: string) => Promise<boolean>;
  canRename: boolean;
  /**
   * 按 terminal 解析等待/完成数字。页面可注入；缺省读全局 hint Context。
   */
  resolveHint?: (sessionId: string) => AgentHintCounts;
}

/**
 * Business Logic（为什么需要这个组件）:
 *   用户在 Workbench 顶部切换/关闭/新建 terminal window，键盘用户需要 roving tab 语义；
 *   每个 tab 只显示窗口名,不再展示 agent provider/phase（避免挤压窗口名）。
 *
 * Code Logic（这个组件做什么）:
 *   渲染 tablist + 新建按钮；处理 Arrow/Home/End 与 close 后焦点；session.name 自然撑满剩余空间。
 */
export function WorkbenchSessionTabs({
  sessions,
  activeSessionId,
  sessionBusy,
  canCreate,
  onFocusSession,
  onCloseSession,
  onCreateSession,
  onRenameSession,
  canRename,
  resolveHint,
}: WorkbenchSessionTabsProps): ReactElement {
  const { t } = useTranslation(['workbench']);
  const hintContext = useOptionalWorkbenchAgentHints();
  const resolveHintCounts = resolveHint ?? hintContext?.hintsForTerminal;

  // Business Logic: 双击 tab 名进入行内重命名；draft 仅活在本地，避免与 inspector 的
  // sessionNameDraft 耦合。pendingActionRef 记录本轮编辑如何结束（commit/cancel），让随后由
  // 卸载或失焦触发的 blur 不重复提交、也不把取消误当提交。
  const [editingSessionId, setEditingSessionId] = useState<string | null>(null);
  const [editingDraft, setEditingDraft] = useState<string>('');
  const renameInputRef = useRef<HTMLInputElement>(null);
  const pendingActionRef = useRef<'commit' | 'cancel' | null>(null);

  // Code Logic: 进入编辑态时聚焦并全选原名，方便整体覆盖。
  useEffect(() => {
    if (editingSessionId && renameInputRef.current) {
      renameInputRef.current.focus();
      renameInputRef.current.select();
    }
  }, [editingSessionId]);

  // 注：远端掉线 / 被编辑 session 消失时不在此处 effect setState（react-hooks/set-state-in-effect）；
  // 安全由下游兜底——commitRename → onRenameSession → 控制器 renameSessionById 在 remoteWriteDisabled
  // 时短路返回 false（不保存）；session 消失则条件渲染自然不产出 input，stale editingSessionId 在下次双击时重置。

  const commitRename = useCallback(
    (sessionId: string, draft: string): void => {
      pendingActionRef.current = 'commit';
      setEditingSessionId(null);
      const trimmed = draft.trim();
      const current = sessions.find((session) => session.id === sessionId);
      // 空 / 仅空白 / 未改名 → no-op，不调 rename。
      if (trimmed && current && trimmed !== current.name) {
        void onRenameSession(sessionId, trimmed);
      }
      setEditingDraft('');
      focusElementById(sessionTabButtonId(sessionId));
    },
    [onRenameSession, sessions],
  );

  const cancelRename = useCallback((sessionId: string): void => {
    pendingActionRef.current = 'cancel';
    setEditingSessionId(null);
    setEditingDraft('');
    focusElementById(sessionTabButtonId(sessionId));
  }, []);

  const handleNameDoubleClick = useCallback(
    (event: MouseEvent<HTMLSpanElement>, sessionId: string, currentName: string): void => {
      // 阻止冒泡，避免触发 button onClick 的 focus-session（单/双击第一次 click 已聚焦过）。
      event.stopPropagation();
      if (!canRename) return;
      // 新一轮编辑：清掉上一轮残留的 pending 标记。
      pendingActionRef.current = null;
      setEditingSessionId(sessionId);
      setEditingDraft(currentName);
    },
    [canRename],
  );

  const handleInputBlur = useCallback(
    (sessionId: string, draft: string): void => {
      // 若已由 Enter(commit) 或 Escape(cancel) 结束本轮编辑，blur 只复位标记，不重复动作。
      if (pendingActionRef.current !== null) {
        pendingActionRef.current = null;
        return;
      }
      commitRename(sessionId, draft);
    },
    [commitRename],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   键盘用户在 session tablist 内用方向键/Home/End 切换 window。
   *
   * Code Logic（这个函数做什么）:
   *   getRovingTabIndex 求下一索引，onFocusSession 后 focus 对应 button。
   */
  const handleSessionTabKeyDown = useCallback(
    (event: KeyboardEvent<HTMLButtonElement>, sessionId: string) => {
      const key = event.key;
      if (key !== 'ArrowLeft' && key !== 'ArrowRight' && key !== 'Home' && key !== 'End') {
        return;
      }
      if (sessions.length === 0) return;
      event.preventDefault();
      const currentIndex = sessions.findIndex((session) => session.id === sessionId);
      if (currentIndex < 0) return;
      const nextIndex = getRovingTabIndex(currentIndex, key as RovingTabKey, sessions.length);
      const nextSession = sessions[nextIndex];
      if (!nextSession) return;
      onFocusSession(nextSession.id);
      focusElementById(sessionTabButtonId(nextSession.id));
    },
    [onFocusSession, sessions],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   关闭选中 window 后焦点应落到相邻 tab 或新建按钮。
   *
   * Code Logic（这个函数做什么）:
   *   关闭前计算目标 id，await onCloseSession 后 focus；不改 close 排序语义。
   */
  const handleCloseSessionTab = useCallback(
    async (sessionId: string): Promise<void> => {
      const index = sessions.findIndex((session) => session.id === sessionId);
      const wasSelected = sessionId === activeSessionId;
      let focusTargetId: string | null = null;
      if (wasSelected) {
        if (sessions.length <= 1) {
          focusTargetId = NEW_SESSION_BUTTON_ID;
        } else if (index >= 0 && index < sessions.length - 1) {
          focusTargetId = sessionTabButtonId(sessions[index + 1].id);
        } else if (index > 0) {
          focusTargetId = sessionTabButtonId(sessions[index - 1].id);
        } else {
          focusTargetId = NEW_SESSION_BUTTON_ID;
        }
      }
      await onCloseSession(sessionId);
      if (focusTargetId) {
        focusElementById(focusTargetId);
      }
    },
    [activeSessionId, onCloseSession, sessions],
  );

  const summary = sessions.reduce(
    (acc, session) => {
      const hint = resolveHintCounts?.(session.id) ?? EMPTY_HINT_COUNTS;
      return hintCountsFrom(
        acc.waitingCount + hint.waitingCount,
        acc.stoppedCount + hint.stoppedCount,
      );
    },
    EMPTY_HINT_COUNTS,
  );
  const summaryAria = agentHintAriaSpec(summary);

  return (
    <div className={styles.sessionTabs} role="tablist" aria-label={t('workbench:terminalTabs')}>
      <HintStatusDot
        className={styles.sessionHintSummary}
        waitingCount={summary.waitingCount}
        stoppedCount={summary.stoppedCount}
        aria-label={t('workbench:agentHints.navSummary', {
          waiting: summary.waitingCount,
          stopped: summary.stoppedCount,
        })}
        data-testid="workbench-session-hint-summary"
        title={t(summaryAria.key, summaryAria.values)}
      />
      {sessions.map((session) => {
        const selected = session.id === activeSessionId;
        const hint = resolveHintCounts?.(session.id) ?? EMPTY_HINT_COUNTS;
        const hintAria = agentHintAriaSpec(hint);
        return (
          <div
            key={session.id}
            className={styles.sessionTab}
            data-active={selected || undefined}
          >
            {editingSessionId === session.id ? (
              <>
                <HintStatusDot
                  className={styles.sessionDot}
                  data-status={session.status}
                  waitingCount={hint.waitingCount}
                  stoppedCount={hint.stoppedCount}
                  aria-label={t(hintAria.key, hintAria.values)}
                />
                <input
                  ref={renameInputRef}
                  className={styles.sessionNameInput}
                  value={editingDraft}
                  aria-label={t('workbench:renameSession')}
                  placeholder={t('workbench:sessionNamePlaceholder')}
                  onChange={(event) => setEditingDraft(event.target.value)}
                  onMouseDown={(event) => event.stopPropagation()}
                  onClick={(event) => event.stopPropagation()}
                  onBlur={() => handleInputBlur(session.id, editingDraft)}
                  onKeyDown={(event) => {
                    if (event.key === 'Enter') {
                      event.preventDefault();
                      commitRename(session.id, editingDraft);
                    } else if (event.key === 'Escape') {
                      event.preventDefault();
                      cancelRename(session.id);
                    }
                  }}
                />
              </>
            ) : (
            <button
              id={sessionTabButtonId(session.id)}
              type="button"
              role="tab"
              className={styles.sessionTabButton}
              tabIndex={selected ? 0 : -1}
              aria-selected={selected}
              onClick={() => onFocusSession(session.id)}
              onKeyDown={(event) => handleSessionTabKeyDown(event, session.id)}
            >
              <HintStatusDot
                className={styles.sessionDot}
                data-status={session.status}
                waitingCount={hint.waitingCount}
                stoppedCount={hint.stoppedCount}
                aria-label={t(hintAria.key, hintAria.values)}
              />
              <span
                className={styles.sessionName}
                title={canRename ? t('workbench:renameSessionHint') : undefined}
                onDoubleClick={(event) =>
                  handleNameDoubleClick(event, session.id, session.name)
                }
              >
                {session.name}
              </span>
            </button>
            )}
            <Button
              variant="icon"
              icon={<XIcon />}
              title={t('workbench:closeTerminal')}
              aria-label={t('workbench:closeTerminal')}
              onClick={(event) => {
                event.stopPropagation();
                void handleCloseSessionTab(session.id);
              }}
            />
          </div>
        );
      })}
      <Button
        id={NEW_SESSION_BUTTON_ID}
        className={styles.newSessionButton}
        variant="secondary"
        size="sm"
        icon={<PlusIcon />}
        title={t('workbench:newSession')}
        aria-label={t('workbench:newSession')}
        data-workbench-responsive-action="true"
        loading={sessionBusy}
        disabled={!canCreate}
        onClick={() => onCreateSession()}
      >
        <span data-workbench-responsive-label="true">{t('workbench:newSession')}</span>
      </Button>
    </div>
  );
}
