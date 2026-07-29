/**
 * agentProviderShortLabel 四 provider 合同。
 */

import { describe, expect, test } from 'vitest';
import { agentProviderShortLabel } from './agentPhasePresentation';

describe('agentProviderShortLabel', () => {
  test('maps four built-in providers including openCodeVisible', () => {
    expect(agentProviderShortLabel('claudeCodeVisible')).toBe('Claude');
    expect(agentProviderShortLabel('codexVisible')).toBe('Codex');
    expect(agentProviderShortLabel('genericTerminal')).toBe('Generic');
    expect(agentProviderShortLabel('openCodeVisible')).toBe('OpenCode');
  });
});
