// @vitest-environment jsdom
/**
 * WorkbenchSessionTabs Agent 投影测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   terminal tab 必须用可访问标签展示 phase，且点击只聚焦 session。
 *
 * Code Logic（这个测试做什么）:
 *   渲染含 agent 的 tab，断言 aria-label 与 phase 文案。
 */

import { afterEach, describe, expect, test, vi } from 'vitest';
import { cleanup, render, screen } from '@testing-library/react';
import i18n from 'i18next';
import { I18nextProvider, initReactI18next } from 'react-i18next';
import type { ReactElement } from 'react';

import type { AgentSessionProjection } from '@/lib/types/agentRuntime';
import type { WorkbenchSession } from '@/lib/types';
import { WorkbenchSessionTabs } from './WorkbenchSessionTabs';

const resources = {
  zh: {
    workbench: {
      terminalTabs: '终端',
      closeTerminal: '关闭',
      newSession: '新建',
      agentPhase: {
        launching: 'Agent 启动中',
        working: 'Agent 工作中',
        needsInput: 'Agent 等待输入',
        idle: 'Agent 空闲',
        completed: 'Agent 已完成',
        failed: 'Agent 运行失败',
        disconnected: 'Agent 已断开',
      },
      agentFreshness: {
        cached: '缓存',
        offline: '离线',
        unsupported: '不支持',
      },
    },
  },
};

void i18n.use(initReactI18next).init({
  lng: 'zh',
  resources,
  interpolation: { escapeValue: false },
});

/**
 * Business Logic（为什么需要这个函数）:
 *   测试需挂载 i18n 上下文。
 *
 * Code Logic（这个函数做什么）:
 *   用 I18nextProvider 包裹 children。
 */
function wrap(ui: ReactElement): ReactElement {
  return <I18nextProvider i18n={i18n}>{ui}</I18nextProvider>;
}

/**
 * Business Logic（为什么需要这个工厂）:
 *   tab 测试只需最小 session 字段。
 *
 * Code Logic（这个函数做什么）:
 *   返回 running session。
 */
function makeSession(id = 's1'): WorkbenchSession {
  return {
    id,
    projectId: 'p1',
    worktreeId: 'wt1',
    name: 'Term 1',
    status: 'running',
    command: 'zsh',
    cols: 80,
    rows: 24,
    startedAt: '2026-07-15T00:00:00.000Z',
    exitedAt: null,
    exitCode: null,
    cwd: '/tmp',
    supportsPanes: false,
    paneCount: 1,
  };
}

/**
 * Business Logic（为什么需要这个工厂）:
 *   phase 用例只需改 phase。
 *
 * Code Logic（这个函数做什么）:
 *   构造 AgentSessionProjection。
 */
function makeAgent(partial: Partial<AgentSessionProjection> = {}): AgentSessionProjection {
  return {
    id: 'a1',
    projectId: 'p1',
    terminalSessionId: 's1',
    providerId: 'claudeCodeVisible',
    phase: 'working',
    version: 1,
    lastActivityAt: '2026-07-15T00:00:00.000Z',
    freshness: 'live',
    ...partial,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   统一渲染 SessionTabs 并注入 agent map。
 *
 * Code Logic（这个函数做什么）:
 *   render + resolveAgent。
 */
function renderSessionTab(options: {
  agent?: AgentSessionProjection | null;
  onFocusSession?: (id: string) => void;
}): void {
  const agent = options.agent ?? null;
  render(
    wrap(
      <WorkbenchSessionTabs
        sessions={[makeSession()]}
        activeSessionId="s1"
        sessionBusy={false}
        canCreate
        onFocusSession={options.onFocusSession ?? vi.fn()}
        onCloseSession={async () => undefined}
        onCreateSession={() => undefined}
        resolveAgent={() => agent}
      />,
    ),
  );
}

describe('WorkbenchSessionTabs agent projection', () => {
  afterEach(() => {
    cleanup();
  });

  test.each([
    ['working', 'Agent 工作中'],
    ['needsInput', 'Agent 等待输入'],
    ['failed', 'Agent 运行失败'],
  ] as const)('renders %s with text and aria label', (phase, label) => {
    renderSessionTab({ agent: makeAgent({ phase }) });
    const status = screen.getByLabelText(new RegExp(label));
    expect(status).toBeTruthy();
    expect(status.textContent).toContain(label);
  });

  test('clicking agent status focuses the terminal session only', () => {
    const onFocus = vi.fn();
    renderSessionTab({ agent: makeAgent({ phase: 'needsInput' }), onFocusSession: onFocus });
    const status = screen.getByLabelText(/Agent 等待输入/);
    status.click();
    expect(onFocus).toHaveBeenCalledWith('s1');
  });
});
