/**
 * crossAgentPresentation pure helper tests.
 *
 * Business Logic（为什么需要）:
 *   选择性适配页的目标排除、peer 门闩、preview-before-apply 与 DTO 解析不得漂移。
 *
 * Code Logic（做什么）:
 *   直接断言 pure function；无 React / API。
 */

import { describe, expect, test } from 'vitest';
import {
  adaptModeTone,
  canRunCrossAgentApply,
  canRunCrossAgentPreview,
  canSelectDestination,
  countApplicableDestinations,
  defaultDestinationsForSource,
  destinationCandidates,
  isPeerContextBlocked,
  normalizeAdaptMode,
  parseCrossAgentApplyResults,
  parseCrossAgentPreview,
  sanitizeDestinations,
  toggleDestinationSelection,
  type CrossAgentPreviewReport,
} from './crossAgentPresentation';

function previewFixture(
  overrides: Partial<CrossAgentPreviewReport> = {},
): CrossAgentPreviewReport {
  return {
    source: 'claude',
    kind: 'instruction',
    needsAdaptation: false,
    destinations: [
      {
        destination: 'codex',
        mode: 'shared',
        path: '/tmp/.codex/AGENTS.md',
        renderedHash: 'abc',
        unifiedDiff: 'diff',
        partialBlockers: [],
        canApply: true,
      },
    ],
    ...overrides,
  };
}

describe('destinationCandidates / sanitize', () => {
  test('excludes source from destination candidates', () => {
    expect(destinationCandidates('claude')).toEqual(['codex', 'opencode']);
    expect(destinationCandidates('codex')).toEqual(['claude', 'opencode']);
  });

  test('cannot select source as destination', () => {
    expect(canSelectDestination('claude', 'claude')).toBe(false);
    expect(canSelectDestination('claude', 'codex')).toBe(true);
  });

  test('sanitizeDestinations drops source and duplicates', () => {
    expect(sanitizeDestinations('claude', ['claude', 'codex', 'codex', 'opencode'])).toEqual([
      'codex',
      'opencode',
    ]);
  });

  test('toggleDestinationSelection ignores source target', () => {
    expect(toggleDestinationSelection('claude', ['codex'], 'claude')).toEqual(['codex']);
    expect(toggleDestinationSelection('claude', ['codex'], 'opencode')).toEqual([
      'codex',
      'opencode',
    ]);
    expect(toggleDestinationSelection('claude', ['codex', 'opencode'], 'codex')).toEqual([
      'opencode',
    ]);
  });

  test('defaultDestinationsForSource is all others', () => {
    expect(defaultDestinationsForSource('opencode')).toEqual(['claude', 'codex']);
  });
});

describe('peer and gates', () => {
  test('peer context blocked when deviceId set', () => {
    expect(isPeerContextBlocked(null)).toBe(false);
    expect(isPeerContextBlocked('')).toBe(false);
    expect(isPeerContextBlocked('peer-1')).toBe(true);
  });

  test('preview gate blocks peer, empty markdown, source-in-dest, unconfirmed scope', () => {
    const base = {
      deviceId: null as string | null,
      source: 'claude' as const,
      destinations: ['codex' as const],
      sourceMarkdown: 'Always run tests.',
      busy: false,
      scope: 'user' as const,
      projectKey: null as string | null,
      scopeConfirmed: true,
    };
    expect(canRunCrossAgentPreview(base).ok).toBe(true);

    expect(canRunCrossAgentPreview({ ...base, deviceId: 'peer-a' }).reason).toBe('peerBlocked');
    expect(canRunCrossAgentPreview({ ...base, sourceMarkdown: '  ' }).reason).toBe(
      'emptyMarkdown',
    );
    expect(canRunCrossAgentPreview({ ...base, destinations: [] }).reason).toBe(
      'emptyDestinations',
    );
    expect(
      canRunCrossAgentPreview({ ...base, destinations: ['claude', 'codex'] }).reason,
    ).toBe('sourceInDestinations');
    expect(canRunCrossAgentPreview({ ...base, scopeConfirmed: false }).reason).toBe(
      'scopeUnconfirmed',
    );
    expect(
      canRunCrossAgentPreview({
        ...base,
        scope: 'project',
        projectKey: null,
      }).reason,
    ).toBe('projectKeyRequired');
  });

  test('apply gate requires preview and applicable rows; blocks peer', () => {
    const preview = previewFixture();
    expect(
      canRunCrossAgentApply({ deviceId: null, preview, busy: false }).ok,
    ).toBe(true);
    expect(
      canRunCrossAgentApply({ deviceId: null, preview: null, busy: false }).reason,
    ).toBe('missingPreview');
    expect(
      canRunCrossAgentApply({
        deviceId: null,
        preview: previewFixture({
          destinations: [
            {
              destination: 'codex',
              mode: 'residual',
              path: '/x',
              partialBlockers: ['residual'],
              canApply: false,
            },
          ],
        }),
        busy: false,
      }).reason,
    ).toBe('noApplicable');
    expect(
      canRunCrossAgentApply({ deviceId: 'peer', preview, busy: false }).reason,
    ).toBe('peerBlocked');
  });
});

describe('parse preview/apply', () => {
  test('parses preview report and normalizes unknown mode to residual', () => {
    const parsed = parseCrossAgentPreview({
      source: 'claude',
      kind: 'instruction',
      needsAdaptation: true,
      destinations: [
        {
          destination: 'codex',
          mode: 'shared',
          path: '/a',
          canApply: true,
          partialBlockers: ['x'],
        },
        {
          destination: 'opencode',
          mode: 'weird',
          path: '/b',
          canApply: false,
        },
      ],
    });
    expect(parsed?.needsAdaptation).toBe(true);
    expect(parsed?.destinations[0]?.mode).toBe('shared');
    expect(parsed?.destinations[1]?.mode).toBe('residual');
    expect(normalizeAdaptMode('adapted')).toBe('adapted');
  });

  test('parseCrossAgentApplyResults tolerates bad rows', () => {
    const rows = parseCrossAgentApplyResults([
      { destination: 'codex', status: 'applied', path: '/a' },
      { destination: 'nope', path: '/b' },
      null,
    ]);
    expect(rows).toEqual([
      { destination: 'codex', status: 'applied', path: '/a', errorCode: null },
    ]);
  });

  test('countApplicable and mode tones', () => {
    const preview = previewFixture({
      destinations: [
        {
          destination: 'codex',
          mode: 'shared',
          path: '/a',
          partialBlockers: [],
          canApply: true,
        },
        {
          destination: 'opencode',
          mode: 'residual',
          path: '/b',
          partialBlockers: ['r'],
          canApply: false,
        },
      ],
    });
    expect(countApplicableDestinations(preview)).toBe(1);
    expect(adaptModeTone('shared')).toBe('success');
    expect(adaptModeTone('residual')).toBe('danger');
  });
});
