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

import { useCallback } from 'react';
import type { KeyboardEvent, ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { Button } from '@/components/primitives';
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
}

/**
 * Business Logic（为什么需要这个组件）:
 *   用户在 Workbench 顶部切换/关闭/新建 terminal window，键盘用户需要 roving tab 语义。
 *
 * Code Logic（这个组件做什么）:
 *   渲染 tablist + 新建按钮；处理 Arrow/Home/End 与 close 后焦点。
 */
export function WorkbenchSessionTabs({
  sessions,
  activeSessionId,
  sessionBusy,
  canCreate,
  onFocusSession,
  onCloseSession,
  onCreateSession,
}: WorkbenchSessionTabsProps): ReactElement {
  const { t } = useTranslation(['workbench']);

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

  return (
    <div className={styles.sessionTabs} role="tablist" aria-label={t('workbench:terminalTabs')}>
      {sessions.map((session) => {
        const selected = session.id === activeSessionId;
        return (
          <div
            key={session.id}
            className={styles.sessionTab}
            data-active={selected || undefined}
          >
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
              <span className={styles.sessionDot} data-status={session.status} />
              <span className={styles.sessionName}>{session.name}</span>
            </button>
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
