import { describe, expect, it } from 'vitest';
import {
  allAgentIdentities,
  allHistorySources,
  allHubTargets,
  allSessionSources,
  identityByRuntime,
  parseAgentId,
} from './agentCatalog';

describe('agentCatalog', () => {
  it('registers five product identities', () => {
    expect(allAgentIdentities()).toHaveLength(5);
    expect(allHubTargets()).toEqual(['claude', 'codex', 'opencode', 'grok', 'gemini']);
    expect(allSessionSources()).toEqual(['claude', 'codex', 'opencode', 'grok', 'gemini']);
    expect(allHistorySources()).toEqual(['claude', 'codex', 'opencode', 'grok', 'gemini']);
  });

  it('rejects unknown agent ids', () => {
    expect(parseAgentId('antigravity')).toBeNull();
    expect(parseAgentId('genericTerminal')).toBeNull();
  });

  it('does not map genericTerminal to a product identity', () => {
    expect(identityByRuntime('genericTerminal')).toBeNull();
    expect(identityByRuntime('grokBuildVisible')?.id).toBe('grok');
    expect(identityByRuntime('geminiCliVisible')?.id).toBe('gemini');
  });
});
