import { describe, expect, test, vi } from 'vitest';
import {
  DEFAULT_WORKBENCH_WINDOW_TITLE,
  formatWorkbenchWindowTitle,
  syncWorkbenchWindowTitle,
} from './workbenchWindowTitle';

describe('workbenchWindowTitle', () => {
  test('formats project name and falls back to default', () => {
    expect(formatWorkbenchWindowTitle('demo-app')).toBe('demo-app — cc-partner');
    expect(formatWorkbenchWindowTitle('  ')).toBe(DEFAULT_WORKBENCH_WINDOW_TITLE);
    expect(formatWorkbenchWindowTitle(null)).toBe(DEFAULT_WORKBENCH_WINDOW_TITLE);
  });

  test('sync swallows setTitle failures', async () => {
    const setTitle = vi.fn(async () => {
      throw new Error('no tauri');
    });
    await expect(syncWorkbenchWindowTitle(setTitle, 'p')).resolves.toBeUndefined();
    expect(setTitle).toHaveBeenCalledWith('p — cc-partner');
  });
});
