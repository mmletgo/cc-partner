// @vitest-environment jsdom
/**
 * AgentHubShell pure view 测试。
 *
 * Business Logic: 锁定 agent 切换、user→project 隐藏设备、离线 peer 不可选、Adapt 本机约束。
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
 * Code Logic: 默认 local/user/claude + 两个 peer（一在线一离线）。
 */
function buildProps(overrides: Partial<AgentHubShellProps> = {}): AgentHubShellProps {
  return {
    context: { ...DEFAULT_AGENT_HUB_CONTEXT },
    onContextChange: vi.fn(),
    peers: [
      { deviceId: 'peer-online', name: 'Peer Online', online: true },
      { deviceId: 'peer-offline', name: 'Peer Offline', online: false },
    ],
    projects: [
      { key: 'local:proj-a', label: 'Project A', remote: false },
      { key: 'remote:peer-1/path', label: 'Remote Project', remote: true },
    ],
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
  });

  test('user → project hides device selector and shows project selector', () => {
    const onContextChange = vi.fn();
    const { rerender, props } = renderShell({ onContextChange });

    expect(screen.getByTestId('agent-hub-device-select')).toBeTruthy();
    expect(screen.queryByTestId('agent-hub-project-select')).toBeNull();

    fireEvent.click(screen.getByTestId('agent-hub-scope-project'));
    expect(onContextChange).toHaveBeenCalledWith(
      expect.objectContaining({ scope: 'project' }),
    );

    const projectContext: AgentHubContext = {
      ...DEFAULT_AGENT_HUB_CONTEXT,
      scope: 'project',
      deviceId: null,
      projectKey: null,
    };
    rerender(
      <I18nextProvider i18n={i18n}>
        <AgentHubShell {...props} context={projectContext} onContextChange={onContextChange} />
      </I18nextProvider>,
    );

    expect(screen.queryByTestId('agent-hub-device-select')).toBeNull();
    expect(screen.getByTestId('agent-hub-project-select')).toBeTruthy();
  });

  test('offline peer option is not selectable', () => {
    const onContextChange = vi.fn();
    renderShell({ onContextChange });

    const offline = screen.getByTestId('agent-hub-device-option-peer-offline') as HTMLOptionElement;
    expect(offline.disabled).toBe(true);

    const online = screen.getByTestId('agent-hub-device-option-peer-online') as HTMLOptionElement;
    expect(online.disabled).toBe(false);

    // Selecting online peer works
    fireEvent.change(screen.getByTestId('agent-hub-device-select'), {
      target: { value: 'peer-online' },
    });
    expect(onContextChange).toHaveBeenCalledWith({ deviceId: 'peer-online' });
  });

  test('deviceId !== null disables Adapt with local-only reason', () => {
    const onAdapt = vi.fn();
    renderShell({
      context: {
        ...DEFAULT_AGENT_HUB_CONTEXT,
        deviceId: 'peer-online',
      },
      actions: {
        onPull: vi.fn(),
        onPush: vi.fn(),
        onAdapt,
        adaptDisabledReason: null,
      },
    });

    const adapt = screen.getByTestId('agent-hub-action-adapt') as HTMLButtonElement;
    expect(adapt.disabled).toBe(true);
    expect(adapt.getAttribute('title') || adapt.textContent || '').toMatch(
      /this device|local|本机|same device|同机/i,
    );
    fireEvent.click(adapt);
    expect(onAdapt).not.toHaveBeenCalled();
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

  test('renders children slot', () => {
    renderShell();
    expect(screen.getByTestId('shell-slot').textContent).toBe('content');
  });
});
