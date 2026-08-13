// @vitest-environment jsdom
/**
 * 工作台标语纯逻辑：存储、Markdown 子集、字号拟合。
 */
import { afterEach, describe, expect, it } from 'vitest';
import {
  BANNER_CHANGE_EVENT,
  BANNER_MAX_CHARS,
  BANNER_MAX_FONT_PX,
  BANNER_MIN_FONT_PX,
  BANNER_STORAGE_KEY,
  clampBannerMarkdown,
  fitBannerFontSize,
  isSafeHttpUrl,
  parseBannerMarkdown,
  readWorkbenchBanner,
  writeWorkbenchBanner,
} from './workbenchBanner';

afterEach(() => {
  window.localStorage.removeItem(BANNER_STORAGE_KEY);
});

describe('clampBannerMarkdown', () => {
  it('keeps text within the UTF-16 budget', () => {
    expect(clampBannerMarkdown('ok')).toBe('ok');
    const overflow = `${'a'.repeat(BANNER_MAX_CHARS)}🎉`;
    const clamped = clampBannerMarkdown(overflow);
    expect(clamped.length).toBe(BANNER_MAX_CHARS);
    expect(clamped.endsWith('🎉')).toBe(false);
  });
});

describe('isSafeHttpUrl', () => {
  it('allows only absolute http(s) urls', () => {
    expect(isSafeHttpUrl('https://example.com/x')).toBe(true);
    expect(isSafeHttpUrl('http://localhost:5173')).toBe(true);
    expect(isSafeHttpUrl('javascript:alert(1)')).toBe(false);
    expect(isSafeHttpUrl('/relative')).toBe(false);
    expect(isSafeHttpUrl('https://example.com/path with space')).toBe(false);
  });
});

describe('parseBannerMarkdown', () => {
  it('parses the supported inline subset and keeps unsupported syntax as text', () => {
    expect(parseBannerMarkdown('**bold** *em* ~~del~~ `code`')).toEqual([
      { type: 'strong', value: 'bold' },
      { type: 'text', value: ' ' },
      { type: 'em', value: 'em' },
      { type: 'text', value: ' ' },
      { type: 'del', value: 'del' },
      { type: 'text', value: ' ' },
      { type: 'code', value: 'code' },
    ]);

    expect(parseBannerMarkdown('see [docs](https://example.com) now')).toEqual([
      { type: 'text', value: 'see ' },
      { type: 'link', text: 'docs', href: 'https://example.com' },
      { type: 'text', value: ' now' },
    ]);

    expect(parseBannerMarkdown('line1\nline2')).toEqual([
      { type: 'text', value: 'line1' },
      { type: 'break' },
      { type: 'text', value: 'line2' },
    ]);

    expect(parseBannerMarkdown('# title\n- item\n<script>x</script>')).toEqual([
      { type: 'text', value: '# title' },
      { type: 'break' },
      { type: 'text', value: '- item' },
      { type: 'break' },
      { type: 'text', value: '<script>x</script>' },
    ]);

    expect(parseBannerMarkdown('**unclosed and [bad](javascript:alert(1))')).toEqual([
      { type: 'text', value: '**unclosed and [bad](javascript:alert(1))' },
    ]);
  });
});

describe('banner storage', () => {
  it('writes a versioned record and reads it back', () => {
    const events: string[] = [];
    const onChange = (event: Event): void => {
      events.push((event as CustomEvent<string>).detail);
    };
    window.addEventListener(BANNER_CHANGE_EVENT, onChange);
    writeWorkbenchBanner('  ship it 🎉  ');
    window.removeEventListener(BANNER_CHANGE_EVENT, onChange);

    expect(readWorkbenchBanner()).toBe('  ship it 🎉  ');
    expect(events).toEqual(['  ship it 🎉  ']);
    const stored = JSON.parse(window.localStorage.getItem(BANNER_STORAGE_KEY) ?? '{}') as {
      version: number;
      markdown: string;
    };
    expect(stored.version).toBe(1);
    expect(stored.markdown).toBe('  ship it 🎉  ');
  });

  it('treats invalid or foreign payloads as empty', () => {
    window.localStorage.setItem(BANNER_STORAGE_KEY, '{');
    expect(readWorkbenchBanner()).toBe('');

    window.localStorage.setItem(BANNER_STORAGE_KEY, JSON.stringify({ version: 2, markdown: 'x' }));
    expect(readWorkbenchBanner()).toBe('');

    window.localStorage.setItem(BANNER_STORAGE_KEY, JSON.stringify({ version: 1, markdown: 3 }));
    expect(readWorkbenchBanner()).toBe('');
  });
});

describe('fitBannerFontSize', () => {
  it('picks the largest size that still fits the box', () => {
    const size = fitBannerFontSize({
      maxWidth: 200,
      maxHeight: 60,
      measure: (px) => ({ width: px * 8, height: px * 1.2 }),
    });
    expect(size).toBe(25);
    expect(size).toBeLessThanOrEqual(BANNER_MAX_FONT_PX);
  });

  it('returns the minimum when even the smallest size overflows', () => {
    expect(
      fitBannerFontSize({
        maxWidth: 20,
        maxHeight: 20,
        measure: () => ({ width: 400, height: 400 }),
      }),
    ).toBe(BANNER_MIN_FONT_PX);
  });

  it('returns the minimum for a collapsed box', () => {
    expect(
      fitBannerFontSize({
        maxWidth: 0,
        maxHeight: 48,
        measure: () => ({ width: 1, height: 1 }),
      }),
    ).toBe(BANNER_MIN_FONT_PX);
  });
});
