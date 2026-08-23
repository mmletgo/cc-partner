// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeAll, describe, expect, test, vi } from 'vitest';
import { I18nextProvider } from 'react-i18next';
import i18n from '@/i18n';
import type { WorkbenchProject } from '@/lib/types';
import { MobileProjectPanel } from './MobileProjectPanel';

vi.mock('@/hooks/useLanAgentFleet', () => ({
  useLanAgentFleet: () => ({ snapshot: null, projectSummaries: {} }),
}));

function createProject(id: string): WorkbenchProject {
  return {
    id,
    name: `Project ${id}`,
    kind: 'local',
    deviceId: 'self',
    deviceName: 'Mac',
    path: `/tmp/${id}`,
    lastOpenedAt: '2026-08-24T00:00:00.000Z',
    createdAt: '2026-08-24T00:00:00.000Z',
    updatedAt: '2026-08-24T00:00:00.000Z',
  };
}

describe('MobileProjectPanel', () => {
  beforeAll(async () => {
    await i18n.changeLanguage('zh');
  });

  afterEach(() => {
    cleanup();
  });

  test('exposes add local/lan actions and asks to remove from the row menu', () => {
    const onAddLocal = vi.fn();
    const onAddRemote = vi.fn();
    const onRemoveRequest = vi.fn();
    render(
      <I18nextProvider i18n={i18n}>
        <MobileProjectPanel
          projects={[createProject('p1')]}
          activeProjectId={null}
          loading={false}
          error={null}
          onSelect={vi.fn()}
          onRefresh={vi.fn()}
          onAddLocal={onAddLocal}
          onAddRemote={onAddRemote}
          onRemoveRequest={onRemoveRequest}
        />
      </I18nextProvider>,
    );

    fireEvent.click(screen.getByRole('button', { name: '添加本机项目' }));
    fireEvent.click(screen.getByRole('button', { name: '选择局域网项目' }));
    fireEvent.click(screen.getByRole('button', { name: '项目操作' }));
    expect(onAddLocal).toHaveBeenCalledTimes(1);
    expect(onAddRemote).toHaveBeenCalledTimes(1);
    expect(onRemoveRequest).toHaveBeenCalledWith(expect.objectContaining({ id: 'p1' }));
  });
});
