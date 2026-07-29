// @vitest-environment jsdom
/**
 * PluginComponentsDrawer 渲染合同。
 *
 * Business Logic: mixed package / delete preview / partial blockers 必须可见。
 * Code Logic: RTL 渲染 pure props。
 */

import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, test, vi } from 'vitest';
import type { PluginPackageReport } from '@/lib/types/agentHub';
import { PluginComponentsDrawer } from './PluginComponentsDrawer';

vi.mock('react-i18next', () => ({
  useTranslation: () => ({
    t: (key: string, opts?: Record<string, string>) => {
      if (opts?.state) return `${key}:${opts.state}`;
      if (opts?.reason) return `${key}:${opts.reason}`;
      return key;
    },
  }),
}));

function reportFixture(): PluginPackageReport {
  return {
    packageAssetId: 'pkg-1',
    packageDisplayName: 'mixed-plugin',
    sourceTarget: 'claude',
    aggregateStatus: 'partial',
    activationState: 'planned',
    diagnostics: ['diag-1'],
    partialBlockers: ['Skill A@codex:portable_partial', 'Hook B@opencode:hook_mapping_absent'],
    components: [
      {
        kind: 'skill',
        assetId: 'c1',
        displayName: 'Skill A',
        canonicalRevisionId: 'rev-1',
        ownership: 'packageOwned',
        sourceTarget: 'claude',
        targets: [
          {
            target: 'claude',
            status: 'verified',
            reasons: [],
            projectedPaths: ['a'],
            materializedAlias: 'skill-a',
          },
          {
            target: 'codex',
            status: 'partial',
            reasons: ['portable_partial'],
            projectedPaths: [],
          },
          {
            target: 'opencode',
            status: 'sourceOnly',
            reasons: ['source_only'],
            projectedPaths: [],
          },
        ],
      },
      {
        kind: 'hook',
        assetId: 'c2',
        displayName: 'Hook B',
        canonicalRevisionId: 'rev-2',
        ownership: 'shared',
        sourceTarget: 'claude',
        residualReason: 'targetOnly_no_mapping',
        targets: [
          {
            target: 'claude',
            status: 'verified',
            reasons: [],
            projectedPaths: ['b'],
          },
          {
            target: 'codex',
            status: 'blocked',
            reasons: ['hook_mapping_absent'],
            projectedPaths: [],
          },
          {
            target: 'opencode',
            status: 'blocked',
            reasons: ['hook_mapping_absent'],
            projectedPaths: [],
          },
        ],
      },
    ],
    residuals: [
      {
        residualTarget: 'claude',
        residualKind: 'runtime',
        treeManifestHash: 'h1',
        included: true,
        reasons: [],
      },
    ],
    deletePreview: {
      packageAssetId: 'pkg-1',
      packageDisplayName: 'mixed-plugin',
      components: [
        {
          assetId: 'c1',
          displayName: 'Skill A',
          kind: 'skill',
          ownership: 'packageOwned',
          decision: 'tombstoneOwned',
        },
        {
          assetId: 'c2',
          displayName: 'Hook B',
          kind: 'hook',
          ownership: 'shared',
          decision: 'preserveShared',
        },
      ],
    },
  };
}

afterEach(() => {
  cleanup();
});

describe('PluginComponentsDrawer', () => {
  test('renders mixed package with per-component matrices and exact blockers', () => {
    render(
      <PluginComponentsDrawer open report={reportFixture()} onClose={() => undefined} />,
    );

    expect(screen.getByTestId('plugin-components-drawer')).toBeTruthy();
    expect(screen.getByTestId('plugin-package-aggregate').getAttribute('data-aggregate')).toBe(
      'partial',
    );
    expect(screen.getByTestId('plugin-package-not-synced')).toBeTruthy();
    expect(screen.getByTestId('plugin-package-partial-blockers').textContent).toContain(
      'Skill A@codex:portable_partial',
    );
    expect(screen.getByTestId('plugin-package-partial-blockers').textContent).toContain(
      'Hook B@opencode:hook_mapping_absent',
    );

    expect(screen.getByTestId('plugin-component-c1').getAttribute('data-revision')).toBe('rev-1');
    expect(screen.getByTestId('plugin-component-c1').getAttribute('data-ownership')).toBe(
      'packageOwned',
    );
    expect(screen.getByTestId('plugin-component-cell-c1-claude').getAttribute('data-status')).toBe(
      'verified',
    );
    expect(screen.getByTestId('plugin-component-cell-c1-codex').getAttribute('data-status')).toBe(
      'partial',
    );
    expect(screen.getByTestId('plugin-component-cell-c2-opencode').getAttribute('data-status')).toBe(
      'blocked',
    );
    expect(screen.getByTestId('plugin-component-residual-c2').textContent).toContain(
      'targetOnly_no_mapping',
    );
  });

  test('delete preview lists tombstone versus preserve', () => {
    render(
      <PluginComponentsDrawer open report={reportFixture()} onClose={() => undefined} />,
    );
    expect(screen.getByTestId('plugin-delete-tombstone').textContent).toContain('Skill A');
    expect(screen.getByTestId('plugin-delete-preserve').textContent).toContain('Hook B');
  });
});
