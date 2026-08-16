import { describe, expect, it } from 'vitest';
import {
  allAgentIdentities,
  allHistorySources,
  allHubTargets,
  allSessionSources,
  headlessOptimizerProviders,
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

  it('offers only implemented headless optimizer providers', () => {
    expect(headlessOptimizerProviders().map((row) => row.id)).toEqual(['claude', 'grok']);
  });

  it('does not map genericTerminal to a product identity', () => {
    expect(identityByRuntime('genericTerminal')).toBeNull();
    expect(identityByRuntime('grokBuildVisible')?.id).toBe('grok');
    expect(identityByRuntime('geminiCliVisible')?.id).toBe('gemini');
  });
});
