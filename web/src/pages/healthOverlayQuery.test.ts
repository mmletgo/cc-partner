import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { describe, expect, test } from 'vitest';
import { resolveOverlayTemplateId } from '@/lib/healthReminders';
import { computeRestLeft } from './healthOverlayCountdown';

describe('HealthOverlay query helpers', () => {
  test('legacy type query maps to builtin template ids', () => {
    const search = (entries: Record<string, string>) => ({
      get: (key: string) => entries[key] ?? null,
    });
    expect(resolveOverlayTemplateId(search({ template: 'kegel' }))).toBe('kegel');
    expect(resolveOverlayTemplateId(search({ type: 'water' }))).toBe('water');
    expect(resolveOverlayTemplateId(search({ type: 'reminder' }))).toBe('rest');
  });

  test('countdown never goes negative', () => {
    expect(computeRestLeft(100, 90)).toBe(10);
    expect(computeRestLeft(100, 120)).toBe(0);
  });

  test('overlay scrim uses a theme-stable dark veil, not --fg mix', () => {
    const css = readFileSync(
      fileURLToPath(new URL('./HealthOverlay.module.css', import.meta.url)),
      'utf8',
    );
    if (css.includes('var(--fg)')) {
      throw new Error('HealthOverlay must not mix --fg into the fullscreen scrim (dark theme --fg is cream)');
    }
    expect(css).toContain('var(--overlay-scrim)');
    expect(css).toContain('var(--overlay-on)');
  });
});
