// @vitest-environment jsdom
/**
 * AgentAdapterCatalogStrip 只列出 effectively available Agent。
 */

import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, test } from 'vitest';
import type { OrchestratorAgentAdapterCatalogItem } from '@/lib/types';
import { AgentAdapterCatalogStrip } from './AgentAdapterCatalogStrip';

function adapter(
  provider: string,
  overrides: Partial<OrchestratorAgentAdapterCatalogItem> = {},
): OrchestratorAgentAdapterCatalogItem {
  return {
    provider,
    available: true,
    completionContract: 'hookEvent',
    supportsResume: true,
    supportsUsage: true,
    ...overrides,
  };
}

afterEach(() => cleanup());

describe('AgentAdapterCatalogStrip', () => {
  test('hides OpenCode when bridge is missing (not effectively available)', () => {
    render(
      <AgentAdapterCatalogStrip
        agentAdapters={[adapter('openCodeVisible', { available: true, bridgeStatus: undefined })]}
      />,
    );
    expect(screen.queryByTestId('agent-adapter-catalog-strip')).toBeNull();
    expect(screen.queryByTestId('agent-adapter-openCodeVisible')).toBeNull();
  });

  test('ready OpenCode still presents as available', () => {
    render(
      <AgentAdapterCatalogStrip
        agentAdapters={[adapter('openCodeVisible', { available: true, bridgeStatus: 'ready' })]}
      />,
    );
    const row = screen.getByTestId('agent-adapter-openCodeVisible');
    expect(row.getAttribute('data-effectively-available')).toBe('true');
    expect(row.getAttribute('data-bridge-status')).toBe('ready');
    expect(row.getAttribute('data-completion')).toBe('hookEvent');
  });

  test('lists only effectively available adapters from a mixed catalog', () => {
    render(
      <AgentAdapterCatalogStrip
        agentAdapters={[
          adapter('claudeCodeVisible', { available: true }),
          adapter('codexVisible', { available: false }),
          adapter('openCodeVisible', { available: true, bridgeStatus: 'previewRequired' }),
          adapter('geminiCliVisible', { available: true }),
        ]}
      />,
    );
    expect(screen.getByTestId('agent-adapter-claudeCodeVisible')).toBeTruthy();
    expect(screen.getByTestId('agent-adapter-geminiCliVisible')).toBeTruthy();
    expect(screen.queryByTestId('agent-adapter-codexVisible')).toBeNull();
    expect(screen.queryByTestId('agent-adapter-openCodeVisible')).toBeNull();
  });

  test('returns null when every adapter is unavailable', () => {
    render(
      <AgentAdapterCatalogStrip
        agentAdapters={[
          adapter('codexVisible', { available: false }),
          adapter('genericTerminal', { available: false }),
        ]}
      />,
    );
    expect(screen.queryByTestId('agent-adapter-catalog-strip')).toBeNull();
  });
});
