// @vitest-environment jsdom
/** MobileProviderPanel：首次失败后由用户事件重试的回归测试。 */
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { I18nextProvider } from 'react-i18next';
import i18n from '@/i18n';
import type { ProviderManagerSummary } from '@/lib/types/providerManager';
import { MobileProviderPanel } from './MobileProviderPanel';

const apiMocks = vi.hoisted(() => ({ getJson: vi.fn(), postJson: vi.fn() }));

vi.mock('@/api/workbenchHttp', () => ({
  getJson: apiMocks.getJson,
  postJson: apiMocks.postJson,
}));

const summary: ProviderManagerSummary = {
  ccSwitchDbPresent: true,
  cli: { available: true, path: '/usr/local/bin/cc-switch', version: '1.0.0' },
  gui: null,
  apps: [
    {
      app: 'claude',
      currentProviderId: 'provider-1',
      providers: [
        { id: 'provider-1', name: 'Provider One', category: null, isCurrent: true },
      ],
    },
  ],
};

describe('MobileProviderPanel', () => {
  beforeEach(async () => {
    apiMocks.getJson.mockReset();
    apiMocks.postJson.mockReset();
    await i18n.changeLanguage('en');
  });

  afterEach(() => cleanup());

  test('recovers from initial failure through an explicit recheck', async () => {
    apiMocks.getJson
      .mockRejectedValueOnce(new Error('offline'))
      .mockResolvedValueOnce({ protocol_version: 1, capabilities: ['provider-manager.v1'] })
      .mockResolvedValueOnce(summary);
    render(
      <I18nextProvider i18n={i18n}>
        <MobileProviderPanel />
      </I18nextProvider>,
    );

    fireEvent.click(await screen.findByRole('button', { name: 'Recheck' }));

    expect(await screen.findByText('Provider One')).toBeTruthy();
    expect(apiMocks.getJson).toHaveBeenCalledTimes(3);
  });
});
