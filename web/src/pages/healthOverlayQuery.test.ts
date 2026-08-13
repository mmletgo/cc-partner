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
});
