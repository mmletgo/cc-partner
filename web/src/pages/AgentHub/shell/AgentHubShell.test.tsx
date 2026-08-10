// @vitest-environment jsdom
/**
 * AgentHubShell pure view 测试。
 *
 * Business Logic: 锁定本机用户级壳层、能力门禁与三组 roving 键盘合同。
 * Code Logic: 注入 props；无 @/api；I18nextProvider 仅渲染标签。
 */

import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { afterEach, beforeAll, describe, expect, test, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { I18nextProvider } from 'react-i18next';
import i18n from '@/i18n';
import {
  DEFAULT_AGENT_HUB_CONTEXT,
  type AgentHubContext,
} from '../context/agentHubContext';
import { AgentHubShell, type AgentHubShellProps } from './AgentHubShell';

const shellDir = dirname(fileURLToPath(import.meta.url));

beforeAll(async () => {
  await i18n.changeLanguage('en');
});

afterEach(() => {
  cleanup();
});

/**
 * Business Logic: 构造可交互的 shell 快照。
 * Code Logic: 默认 local/user/claude；外部上下文仅用于验证 fail-closed。
 */
function buildProps(overrides: Partial<AgentHubShellProps> = {}): AgentHubShellProps {
  return {
    context: { ...DEFAULT_AGENT_HUB_CONTEXT },
    onContextChange: vi.fn(),
    actions: {
      onPull: vi.fn(),
      onPush: vi.fn(),
      onAdapt: vi.fn(),
      adaptDisabledReason: null,
    },
    children: <div data-testid="shell-slot">content</div>,
    ...overrides,
  };
}

/**
 * Business Logic: 统一挂载 i18n + shell。
 * Code Logic: I18nextProvider 包装。
 */
function renderShell(overrides: Partial<AgentHubShellProps> = {}) {
  const props = buildProps(overrides);
  const result = render(
    <I18nextProvider i18n={i18n}>
      <AgentHubShell {...props} />
    </I18nextProvider>,
  );
  return { ...result, props };
}

describe('AgentHubShell', () => {
  test('pure view source does not import @/api/', () => {
    const source = readFileSync(resolve(shellDir, './AgentHubShell.tsx'), 'utf8');
    expect(source).not.toMatch(/from\s+['"]@\/api\//);
  });

  test('click Codex emits onContextChange({ agent: "codex" })', () => {
    const onContextChange = vi.fn();
    renderShell({ onContextChange });
    fireEvent.click(screen.getByTestId('agent-hub-agent-codex'));
    expect(onContextChange).toHaveBeenCalledWith({ agent: 'codex' });
  });

  test('agent switcher remains visible in every instruction lane', () => {
    const { rerender, props } = renderShell();
    expect(screen.getByTestId('agent-hub-agent-switcher')).toBeTruthy();
    for (const instructionLane of ['adapted', 'exclusive'] as const) {
      rerender(
        <I18nextProvider i18n={i18n}>
          <AgentHubShell
            {...props}
            context={{ ...DEFAULT_AGENT_HUB_CONTEXT, instructionLane }}
          />
        </I18nextProvider>,
      );
      expect(screen.getByTestId('agent-hub-agent-switcher')).toBeTruthy();
    }
  });

  test('instructions tab shows lane switcher; skill tab hides it', () => {
    const onContextChange = vi.fn();
    const { rerender, props } = renderShell({ onContextChange });
    expect(screen.getByTestId('agent-hub-lane-switcher')).toBeTruthy();
    fireEvent.click(screen.getByTestId('agent-hub-lane-adapted'));
    expect(onContextChange).toHaveBeenCalledWith({ instructionLane: 'adapted' });

    fireEvent.click(screen.getByTestId('agent-hub-tab-skill'));
    expect(onContextChange).toHaveBeenCalledWith({
      tab: 'skill',
      instructionLane: 'common',
    });

    const skillContext: AgentHubContext = {
      ...DEFAULT_AGENT_HUB_CONTEXT,
      tab: 'skill',
      instructionLane: 'common',
    };
    rerender(
      <I18nextProvider i18n={i18n}>
        <AgentHubShell {...props} context={skillContext} onContextChange={onContextChange} />
      </I18nextProvider>,
    );
    expect(screen.queryByTestId('agent-hub-lane-switcher')).toBeNull();
    // 非 instructions tab 仍显示 agent 导航
    expect(screen.getByTestId('agent-hub-agent-switcher')).toBeTruthy();
  });

  test('scope is fixed local-user and no project/device selector is rendered', () => {
    renderShell();
    expect(screen.getByTestId('agent-hub-fixed-scope')).toBeTruthy();
    expect(screen.queryByTestId('agent-hub-device-select')).toBeNull();
    expect(screen.queryByTestId('agent-hub-project-select')).toBeNull();
    expect(screen.queryByTestId('agent-hub-scope-project')).toBeNull();
  });

  test('peer context is Pull-only and shows visible reasons for blocked actions', () => {
    const onPull = vi.fn();
    const onPush = vi.fn();
    const onAdapt = vi.fn();
    renderShell({
      context: {
        ...DEFAULT_AGENT_HUB_CONTEXT,
        deviceId: 'peer-online',
      },
      actions: {
        onPull,
        onPush,
        onAdapt,
        adaptDisabledReason: null,
      },
    });

    expect((screen.getByTestId('agent-hub-action-pull') as HTMLButtonElement).disabled).toBe(false);
    expect((screen.getByTestId('agent-hub-action-push') as HTMLButtonElement).disabled).toBe(true);
    const adapt = screen.getByTestId('agent-hub-action-adapt') as HTMLButtonElement;
    expect(adapt.disabled).toBe(true);
    expect(screen.getByTestId('agent-hub-push-reason').textContent).toMatch(/Pull-only|只允许 Pull/i);
    expect(screen.getByTestId('agent-hub-adapt-reason').textContent).toMatch(/Pull-only|只允许 Pull/i);
    fireEvent.click(screen.getByTestId('agent-hub-action-pull'));
    fireEvent.click(screen.getByTestId('agent-hub-action-push'));
    fireEvent.click(adapt);
    expect(onPull).toHaveBeenCalledOnce();
    expect(onPush).not.toHaveBeenCalled();
    expect(onAdapt).not.toHaveBeenCalled();
  });

  test('project context disables every toolbar action and explains recovery', () => {
    const onPull = vi.fn();
    const onPush = vi.fn();
    const onAdapt = vi.fn();
    renderShell({
      context: {
        ...DEFAULT_AGENT_HUB_CONTEXT,
        scope: 'project',
        projectKey: 'local:p1',
      },
      actions: { onPull, onPush, onAdapt },
    });

    for (const action of ['pull', 'push', 'adapt']) {
      expect(
        (screen.getByTestId(`agent-hub-action-${action}`) as HTMLButtonElement).disabled,
      ).toBe(true);
    }
    expect(screen.getByTestId('agent-hub-pull-reason').textContent).toMatch(
      /unavailable|暂不可用/i,
    );
    fireEvent.click(screen.getByTestId('agent-hub-action-pull'));
    expect(onPull).not.toHaveBeenCalled();
  });

  test('five tabs emit tab patch; skill selects skill', () => {
    const onContextChange = vi.fn();
    renderShell({ onContextChange });

    fireEvent.click(screen.getByTestId('agent-hub-tab-skill'));
    expect(onContextChange).toHaveBeenCalledWith({
      tab: 'skill',
      instructionLane: 'common',
    });

    fireEvent.click(screen.getByTestId('agent-hub-tab-instructions'));
    expect(onContextChange).toHaveBeenCalledWith({ tab: 'instructions' });
  });

  test('asset tabs show migrated kindCounts from tabCounts prop', () => {
    renderShell({
      tabCounts: { skill: 3, command: 1, mcp: 0, plugin: 2 },
    });

    const skill = screen.getByTestId('agent-hub-tab-skill');
    expect(skill.getAttribute('data-count')).toBe('3');
    expect(skill.textContent).toMatch(/\(3\)/);

    const plugin = screen.getByTestId('agent-hub-tab-plugin');
    expect(plugin.getAttribute('data-count')).toBe('2');
    expect(plugin.textContent).toMatch(/\(2\)/);

    // instructions 不展示数量
    const instructions = screen.getByTestId('agent-hub-tab-instructions');
    expect(instructions.getAttribute('data-count')).toBeNull();
    expect(instructions.textContent).not.toMatch(/\(\d+\)/);
  });

  test('toolbar pull/push invoke action callbacks', () => {
    const onPull = vi.fn();
    const onPush = vi.fn();
    const onAdapt = vi.fn();
    renderShell({
      actions: { onPull, onPush, onAdapt, adaptDisabledReason: null },
    });

    fireEvent.click(screen.getByTestId('agent-hub-action-pull'));
    fireEvent.click(screen.getByTestId('agent-hub-action-push'));
    fireEvent.click(screen.getByTestId('agent-hub-action-adapt'));
    expect(onPull).toHaveBeenCalled();
    expect(onPush).toHaveBeenCalled();
    expect(onAdapt).toHaveBeenCalled();
  });

  test('each group has one tab stop and keyboard navigation wraps with focus + callback', () => {
    const onContextChange = vi.fn();
    renderShell({ onContextChange });

    const tabs = screen.getAllByRole('tab') as HTMLButtonElement[];
    const laneRadios = screen.getByTestId('agent-hub-lane-switcher').querySelectorAll('[role="radio"]');
    const agentRadios = screen.getByTestId('agent-hub-agent-switcher').querySelectorAll('[role="radio"]');
    expect(tabs.filter((node) => node.tabIndex === 0)).toHaveLength(1);
    expect(Array.from(laneRadios).filter((node) => (node as HTMLElement).tabIndex === 0)).toHaveLength(1);
    expect(Array.from(agentRadios).filter((node) => (node as HTMLElement).tabIndex === 0)).toHaveLength(1);

    fireEvent.keyDown(screen.getByTestId('agent-hub-tab-instructions'), { key: 'ArrowLeft' });
    expect(onContextChange).toHaveBeenLastCalledWith({
      tab: 'plugin',
      instructionLane: 'common',
    });
    expect(document.activeElement).toBe(screen.getByTestId('agent-hub-tab-plugin'));

    fireEvent.keyDown(screen.getByTestId('agent-hub-lane-common'), { key: 'End' });
    expect(onContextChange).toHaveBeenLastCalledWith({ instructionLane: 'exclusive' });
    expect(document.activeElement).toBe(screen.getByTestId('agent-hub-lane-exclusive'));

    fireEvent.keyDown(screen.getByTestId('agent-hub-agent-claude'), { key: 'ArrowLeft' });
    expect(onContextChange).toHaveBeenLastCalledWith({ agent: 'opencode' });
    expect(document.activeElement).toBe(screen.getByTestId('agent-hub-agent-opencode'));
  });

  test('active tab controls the labelled tabpanel', () => {
    renderShell();
    const tab = screen.getByTestId('agent-hub-tab-instructions');
    const panel = screen.getByRole('tabpanel');
    expect(tab.getAttribute('aria-controls')).toBe(panel.id);
    expect(panel.getAttribute('aria-labelledby')).toBe(tab.id);
  });

  test('renders children slot', () => {
    renderShell();
    expect(screen.getByTestId('shell-slot').textContent).toBe('content');
  });
});
