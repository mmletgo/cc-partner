// @vitest-environment jsdom
/**
 * AutomationSettingsPanel OpenCode provider catalog 合同。
 *
 * Business Logic: OpenCode 必须展示 completion/bridge/blocked reason，不得伪装 available green。
 * Code Logic: pure panel props + RTL。
 */

import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, test, vi } from 'vitest';
import type { OrchestratorAgentAdapterCatalogItem } from '@/lib/types';
import { AutomationSettingsPanel } from './AutomationSettingsPanel';
import type { AutomationSettingsForm } from './automationSettingsState';

vi.mock('react-i18next', () => ({
  useTranslation: () => ({
    t: (key: string, opts?: Record<string, string>) => {
      if (!opts) return key;
      return `${key}:${Object.values(opts).join('|')}`;
    },
  }),
}));

const baseForm: AutomationSettingsForm = {
  enabled: true,
  maxConcurrentTasks: 1,
  verificationCommandsText: '',
  autoCommit: true,
  autoPushTaskBranch: true,
  autoMergeToMain: true,
  autoPushMain: true,
  notifyHumanReview: true,
  notifyBlocked: true,
  notifyRemoteOutboxFailed: true,
  notifyTaskDone: false,
};

function openCodeItem(
  overrides: Partial<OrchestratorAgentAdapterCatalogItem> = {},
): OrchestratorAgentAdapterCatalogItem {
  return {
    provider: 'openCodeVisible',
    available: false,
    completionContract: 'hookEvent',
    supportsResume: true,
    supportsUsage: true,
    executable: 'opencode',
    version: '0.1.0',
    supportEvidence: 'L3-AGENT-HUB-OPENCODE-RUNTIME-001',
    bridgeStatus: 'previewRequired',
    blockedReason: 'runtime_bridge_required',
    reasonCode: 'l3_runtime_evidence_missing',
    ...overrides,
  };
}

afterEach(() => cleanup());

describe('AutomationSettingsPanel OpenCode catalog', () => {
  test('renders OpenCode with HookEvent, bridge status and blocked reason', () => {
    render(
      <AutomationSettingsPanel
        form={baseForm}
        defaults={baseForm}
        dirty={false}
        saving={false}
        error={null}
        saved={false}
        onChange={() => undefined}
        onResetDefaults={() => undefined}
        onSave={() => undefined}
        agentAdapters={[
          {
            provider: 'claudeCodeVisible',
            available: true,
            completionContract: 'sentinelLine',
            supportsResume: true,
            supportsUsage: true,
          },
          openCodeItem(),
        ]}
      />,
    );

    const row = screen.getByTestId('agent-adapter-openCodeVisible');
    expect(row.getAttribute('data-completion')).toBe('hookEvent');
    expect(row.getAttribute('data-bridge-status')).toBe('previewRequired');
    expect(row.textContent).toContain('runtime_bridge_required');
    expect(row.textContent).toContain('L3-AGENT-HUB-OPENCODE-RUNTIME-001');
    expect(row.textContent).toContain('opencode');
  });
});
