import { describe, expect, test } from 'vitest';
import {
  computeScreenshotToolbarPosition,
  resolveScreenshotOverlayKey,
  SCREENSHOT_TOOLBAR_GAP,
  SCREENSHOT_TOOLBAR_HEIGHT,
  SCREENSHOT_TOOLBAR_WIDTH,
} from './screenshotOverlayInput';

describe('resolveScreenshotOverlayKey', () => {
  test('Escape always cancels', () => {
    expect(resolveScreenshotOverlayKey({ key: 'Escape' }, 'idle')).toBe('cancel');
    expect(resolveScreenshotOverlayKey({ key: 'Escape' }, 'selecting')).toBe('cancel');
    expect(resolveScreenshotOverlayKey({ key: 'Escape' }, 'editing')).toBe('cancel');
  });

  test('Enter confirms only after selection (editing)', () => {
    expect(resolveScreenshotOverlayKey({ key: 'Enter' }, 'editing')).toBe('confirm');
    expect(resolveScreenshotOverlayKey({ key: 'Enter' }, 'idle')).toBe('ignore');
    expect(resolveScreenshotOverlayKey({ key: 'Enter' }, 'selecting')).toBe('ignore');
  });

  test('IME composing or key repeat does not confirm', () => {
    expect(resolveScreenshotOverlayKey({ key: 'Enter', isComposing: true }, 'editing')).toBe(
      'ignore',
    );
    expect(resolveScreenshotOverlayKey({ key: 'Enter', repeat: true }, 'editing')).toBe('ignore');
  });
});

describe('computeScreenshotToolbarPosition', () => {
  const toolbar = { width: SCREENSHOT_TOOLBAR_WIDTH, height: SCREENSHOT_TOOLBAR_HEIGHT };
  const viewport = { width: 1920, height: 1080 };

  test('centers below a mid-screen selection', () => {
    const pos = computeScreenshotToolbarPosition(
      { x: 400, y: 200, w: 500, h: 300 },
      viewport,
      toolbar,
    );
    expect(pos.left).toBe(400 + 250 - SCREENSHOT_TOOLBAR_WIDTH / 2);
    expect(pos.top).toBe(200 + 300 + SCREENSHOT_TOOLBAR_GAP);
  });

  test('clamps to the left when selection hugs the left edge', () => {
    const pos = computeScreenshotToolbarPosition(
      { x: 0, y: 100, w: 80, h: 80 },
      viewport,
      toolbar,
    );
    expect(pos.left).toBe(0);
    expect(pos.top).toBe(100 + 80 + SCREENSHOT_TOOLBAR_GAP);
  });

  test('clamps to the right when selection hugs the right edge', () => {
    const pos = computeScreenshotToolbarPosition(
      { x: 1840, y: 100, w: 80, h: 80 },
      viewport,
      toolbar,
    );
    expect(pos.left).toBe(1920 - SCREENSHOT_TOOLBAR_WIDTH);
    expect(pos.left + SCREENSHOT_TOOLBAR_WIDTH).toBeLessThanOrEqual(1920);
  });

  test('flips above when there is no room below', () => {
    const pos = computeScreenshotToolbarPosition(
      { x: 400, y: 1000, w: 400, h: 70 },
      viewport,
      toolbar,
    );
    expect(pos.top).toBe(1000 - SCREENSHOT_TOOLBAR_HEIGHT - SCREENSHOT_TOOLBAR_GAP);
    expect(pos.top).toBeGreaterThanOrEqual(0);
  });

  test('pins to the viewport bottom when neither side has room', () => {
    const pos = computeScreenshotToolbarPosition(
      { x: 10, y: 0, w: 1900, h: 1080 },
      viewport,
      toolbar,
    );
    expect(pos.top).toBe(1080 - SCREENSHOT_TOOLBAR_HEIGHT);
    expect(pos.left).toBeGreaterThanOrEqual(0);
    expect(pos.left + SCREENSHOT_TOOLBAR_WIDTH).toBeLessThanOrEqual(1920);
  });
});
