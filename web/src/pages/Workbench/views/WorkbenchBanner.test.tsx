// @vitest-environment jsdom
/**
 * WorkbenchBanner：预览/编辑、本机保存、轻量 Markdown。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { I18nextProvider } from 'react-i18next';
import type { ReactElement } from 'react';
import i18n from '@/i18n';
import { BANNER_MAX_CHARS, BANNER_STORAGE_KEY, readWorkbenchBanner } from '../workbenchBanner';
import { WorkbenchBanner } from './WorkbenchBanner';

function wrap(ui: ReactElement): ReactElement {
  return <I18nextProvider i18n={i18n}>{ui}</I18nextProvider>;
}

beforeEach(async () => {
  window.localStorage.removeItem(BANNER_STORAGE_KEY);
  await i18n.changeLanguage('zh');
});

afterEach(() => {
  cleanup();
  window.localStorage.removeItem(BANNER_STORAGE_KEY);
  vi.useRealTimers();
});

describe('WorkbenchBanner', () => {
  it('shows a local placeholder until the user writes a banner', () => {
    render(wrap(<WorkbenchBanner />));
    expect(screen.getByTestId('workbench-banner-preview').textContent).toContain('写一句今日标语');
    expect(window.localStorage.getItem(BANNER_STORAGE_KEY)).toBeNull();
  });

  it('enters edit on click and persists markdown on blur', () => {
    render(wrap(<WorkbenchBanner />));
    fireEvent.click(screen.getByTestId('workbench-banner-preview'));
    const editor = screen.getByTestId('workbench-banner-editor');
    fireEvent.change(editor, { target: { value: '**focus** 🎉' } });
    fireEvent.blur(editor);

    expect(readWorkbenchBanner()).toBe('**focus** 🎉');
    const preview = screen.getByTestId('workbench-banner-preview');
    expect(preview.querySelector('strong')?.textContent).toBe('focus');
    expect(preview.textContent).toContain('🎉');
  });

  it('cancels an unsaved draft with Escape', () => {
    window.localStorage.setItem(
      BANNER_STORAGE_KEY,
      JSON.stringify({ version: 1, markdown: 'kept', updatedAt: 1 }),
    );
    render(wrap(<WorkbenchBanner />));
    fireEvent.click(screen.getByTestId('workbench-banner-preview'));
    const editor = screen.getByTestId('workbench-banner-editor');
    fireEvent.change(editor, { target: { value: 'draft' } });
    fireEvent.keyDown(editor, { key: 'Escape' });

    expect(readWorkbenchBanner()).toBe('kept');
    expect(screen.getByTestId('workbench-banner-preview').textContent).toContain('kept');
  });

  it('renders a safe link without entering edit when the link is clicked', () => {
    window.localStorage.setItem(
      BANNER_STORAGE_KEY,
      JSON.stringify({
        version: 1,
        markdown: 'see [docs](https://example.com) now',
        updatedAt: 1,
      }),
    );
    render(wrap(<WorkbenchBanner />));
    const link = screen.getByRole('link', { name: 'docs' });
    expect(link.getAttribute('href')).toBe('https://example.com');
    expect(link.getAttribute('target')).toBe('_blank');
    fireEvent.mouseDown(link);
    fireEvent.click(link);
    expect(screen.queryByTestId('workbench-banner-editor')).toBeNull();
  });

  it('clamps input at the character budget and announces the limit', () => {
    render(wrap(<WorkbenchBanner />));
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
