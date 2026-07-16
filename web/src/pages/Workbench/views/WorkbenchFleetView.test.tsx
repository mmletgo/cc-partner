// @vitest-environment jsdom
/**
 * WorkbenchFleetView 单元测试。
 */
import type { ReactElement } from 'react';
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import { I18nextProvider } from 'react-i18next';
import i18n from '@/i18n';
import { WorkbenchFleetView } from './WorkbenchFleetView';
import type { LanFleetSnapshot } from '@/lib/types/lanFleet';

/**
 * Business Logic（为什么需要这个函数）:
 *   导航/只读断言需要稳定 fixture。
 *
 * Code Logic（这个函数做什么）:
 *   返回含 live device + project 的 snapshot。
 */
function fleetFixture(): LanFleetSnapshot {
  return {
    generatedAt: '2026-07-15T00:00:00Z',
    truncated: false,
    devices: [
      {
        deviceId: 'd1',
        deviceName: 'Mac Mini',
        reachability: 'live',
        freshness: 'live',
        schedulerSlotsUsed: 1,
        schedulerSlotsMax: 3,
        projects: [
          {
            projectId: 'p1',
            displayName: 'cc-partner',
            projectKind: 'local',
            agentCounts: {
              launching: 0,
              working: 2,
              needsInput: 1,
              idle: 0,
              completed: 0,
              failed: 0,
              disconnected: 0,
            },
            attentionCount: 1,
            terminalCount: 2,
            gitState: 'dirty',
            browserState: 'absent',
            orchestratorRunning: 0,
            orchestratorRetrying: 1,
            lastActivityAt: '2026-07-15T00:01:00Z',
          },
        ],
        errorCode: null,
        capturedAt: '2026-07-15T00:00:00Z',
      },
    ],
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   视图测试需要 Router + i18n。
 *
 * Code Logic（这个函数做什么）:
 *   MemoryRouter + I18nextProvider 包装。
 */
function renderFleet(ui: ReactElement) {
  return render(
    <I18nextProvider i18n={i18n}>
      <MemoryRouter>{ui}</MemoryRouter>
    </I18nextProvider>,
  );
}

beforeAll(async () => {
  await i18n.changeLanguage('zh');
});

afterEach(() => {
  cleanup();
});

describe('WorkbenchFleetView', () => {
  it('offers only navigation actions', () => {
    renderFleet(<WorkbenchFleetView snapshot={fleetFixture()} onRefresh={vi.fn()} />);

    expect(screen.queryByRole('button', { name: /运行|迁移|复制|发送/ })).toBeNull();
    expect(screen.getByRole('link', { name: /打开项目/ })).toBeTruthy();
    expect(screen.getByText('Mac Mini')).toBeTruthy();
    expect(screen.getByText('cc-partner')).toBeTruthy();
  });

  it('shows offline/cached text alternatives', () => {
    const snap = fleetFixture();
    snap.devices[0]!.reachability = 'offline';
    snap.devices[0]!.freshness = 'cached';
    renderFleet(<WorkbenchFleetView snapshot={snap} />);
    expect(screen.getByText('离线')).toBeTruthy();
    expect(screen.getByText('缓存')).toBeTruthy();
  });
});
