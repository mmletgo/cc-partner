// @vitest-environment jsdom
/**
 * AgentAdapterCatalogStrip OpenCode fail-closed 合同。
 */

import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, test, vi } from 'vitest';
import type { OrchestratorAgentAdapterCatalogItem } from '@/lib/types';
import { AgentAdapterCatalogStrip } from './AgentAdapterCatalogStrip';

vi.mock('react-i18next', () => ({
  useTranslation: () => ({
    t: (key: string, opts?: Record<string, string>) => {
      if (!opts) return key;
      return `${key}:${Object.values(opts).join('|')}`;
    },
  }),
}));

function openCode(
  overrides: Partial<OrchestratorAgentAdapterCatalogItem> = {},
): OrchestratorAgentAdapterCatalogItem {
  return {
    provider: 'openCodeVisible',
    available: true,
    completionContract: 'hookEvent',
    supportsResume: true,
    supportsUsage: true,
    executable: 'opencode',
    version: '0.1.0',
    supportEvidence: 'L3-AGENT-HUB-OPENCODE-RUNTIME-001',
    ...overrides,
  };
}

afterEach(() => cleanup());

describe('AgentAdapterCatalogStrip', () => {
  test('missing bridgeStatus is not effectively available green', () => {
    render(
      <AgentAdapterCatalogStrip
        agentAdapters={[openCode({ available: true, bridgeStatus: undefined })]}
        onOpenOpenCodeBridgePreview={vi.fn()}
      />,
    );
    const row = screen.getByTestId('agent-adapter-openCodeVisible');
    expect(row.getAttribute('data-effectively-available')).toBe('false');
    expect(row.getAttribute('data-bridge-status')).toBe('previewRequired');
    expect(row.getAttribute('data-completion')).toBe('hookEvent');
    expect(row.textContent).toContain('hookEvent');
    expect(screen.getByTestId('open-code-bridge-preview-openCodeVisible')).toBeTruthy();
  });

  test('ready bridge can present available', () => {
    render(
      <AgentAdapterCatalogStrip
        agentAdapters={[openCode({ available: true, bridgeStatus: 'ready' })]}
      />,
    );
    const row = screen.getByTestId('agent-adapter-openCodeVisible');
    expect(row.getAttribute('data-effectively-available')).toBe('true');
    expect(row.getAttribute('data-bridge-status')).toBe('ready');
  });
});
