// @vitest-environment jsdom
/**
 * WorkbenchBanner：预览/编辑、owning device 保存、离线只读。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { I18nextProvider } from 'react-i18next';
import type { ReactElement } from 'react';
import i18n from '@/i18n';
import { BANNER_MAX_CHARS, BANNER_STORAGE_KEY, writeWorkbenchBanner } from '../workbenchBanner';
import { WorkbenchBanner } from './WorkbenchBanner';

const bannerApi = vi.hoisted(() => ({
  get: vi.fn(),
  save: vi.fn(),
}));

vi.mock('@/api/workbench', () => ({
  workbenchApi: {
    banner: bannerApi,
  },
}));

function wrap(ui: ReactElement): ReactElement {
  return <I18nextProvider i18n={i18n}>{ui}</I18nextProvider>;
}

beforeEach(async () => {
  window.localStorage.removeItem(BANNER_STORAGE_KEY);
  bannerApi.get.mockReset();
  bannerApi.save.mockReset();
  bannerApi.get.mockResolvedValue({ markdown: '', updatedAt: '' });
  bannerApi.save.mockImplementation(async (markdown: string) => ({
    markdown,
    updatedAt: 't',
  }));
  await i18n.changeLanguage('zh');
});

afterEach(() => {
  cleanup();
  window.localStorage.removeItem(BANNER_STORAGE_KEY);
  vi.useRealTimers();
});

describe('WorkbenchBanner', () => {
  it('shows a local placeholder until the user writes a banner', async () => {
    render(wrap(<WorkbenchBanner />));
    const preview = await screen.findByTestId('workbench-banner-preview');
    expect(preview.textContent).toContain('写一句今日标语');
    await waitFor(() => {
      expect(bannerApi.get).toHaveBeenCalled();
    });
  });

  it('seeds localStorage once when the local row is empty', async () => {
    writeWorkbenchBanner('**focus** 🎉');
    render(wrap(<WorkbenchBanner />));
    await waitFor(() => {
      expect(bannerApi.save).toHaveBeenCalledWith('**focus** 🎉', undefined);
    });
    expect(window.localStorage.getItem(BANNER_STORAGE_KEY)).toBeNull();
  });

  it('enters edit on click and persists markdown on blur', async () => {
    bannerApi.get.mockResolvedValue({ markdown: '', updatedAt: '' });
    render(wrap(<WorkbenchBanner />));
    await waitFor(() => expect(bannerApi.get).toHaveBeenCalled());
    fireEvent.click(screen.getByTestId('workbench-banner-preview'));
    const editor = screen.getByTestId('workbench-banner-editor');
    fireEvent.change(editor, { target: { value: '**focus** 🎉' } });
    fireEvent.blur(editor);

    await waitFor(() => {
      expect(bannerApi.save).toHaveBeenCalledWith('**focus** 🎉', undefined);
    });
    const preview = screen.getByTestId('workbench-banner-preview');
    expect(preview.querySelector('strong')?.textContent).toBe('focus');
    expect(preview.textContent).toContain('🎉');
  });

  it('does not enter edit when remoteWriteDisabled', async () => {
    bannerApi.get.mockResolvedValue({ markdown: 'kept', updatedAt: 't' });
    render(wrap(<WorkbenchBanner remoteWriteDisabled deviceId="peer-1" />));
    await waitFor(() => {
      expect(screen.getByTestId('workbench-banner-preview').textContent).toContain('kept');
    });
    fireEvent.click(screen.getByTestId('workbench-banner-preview'));
    expect(screen.queryByTestId('workbench-banner-editor')).toBeNull();
    expect(screen.getByTestId('workbench-banner-preview').getAttribute('data-readonly')).toBe(
      'true',
    );
  });

  it('cancels an unsaved draft with Escape', async () => {
    bannerApi.get.mockResolvedValue({ markdown: 'kept', updatedAt: 't' });
    render(wrap(<WorkbenchBanner />));
    await waitFor(() => {
      expect(screen.getByTestId('workbench-banner-preview').textContent).toContain('kept');
    });
    fireEvent.click(screen.getByTestId('workbench-banner-preview'));
    const editor = screen.getByTestId('workbench-banner-editor');
    fireEvent.change(editor, { target: { value: 'draft' } });
    fireEvent.keyDown(editor, { key: 'Escape' });

    expect(screen.getByTestId('workbench-banner-preview').textContent).toContain('kept');
  });

  it('clamps input at the character budget and announces the limit', async () => {
    render(wrap(<WorkbenchBanner />));
    await waitFor(() => expect(bannerApi.get).toHaveBeenCalled());
    fireEvent.click(screen.getByTestId('workbench-banner-preview'));
    const overflow = `${'a'.repeat(BANNER_MAX_CHARS)}b`;
    fireEvent.change(screen.getByTestId('workbench-banner-editor'), {
      target: { value: overflow },
    });
    expect((screen.getByTestId('workbench-banner-editor') as HTMLTextAreaElement).value).toBe(
      'a'.repeat(BANNER_MAX_CHARS),
    );
    expect(screen.getByRole('status').textContent).toContain('280');
  });
});
