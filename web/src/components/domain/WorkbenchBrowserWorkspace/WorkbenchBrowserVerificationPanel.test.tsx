/** @vitest-environment jsdom */
import { describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { WorkbenchBrowserVerificationPanel } from './WorkbenchBrowserVerificationPanel';
import type { WorkbenchTransport } from '@/api/workbenchTransport';

vi.mock('react-i18next', () => ({
  useTranslation: () => ({
    t: (key: string) => {
      if (key.includes('verifyCurrent')) return '验证当前预览';
      if (key.includes('unavailable')) return '不可用';
      return key;
    },
  }),
}));

describe('WorkbenchBrowserVerificationPanel', () => {
  it('starts snapshot plus screenshot without asking for selectors', async () => {
    const start = vi.fn().mockResolvedValue({
      session: {
        id: 'run1',
        projectId: 'p',
        previewId: 'p1',
        ownerInstanceId: 'o',
        state: 'succeeded',
        createdAt: '',
        lastActivityAt: '',
        expiresAt: '',
      },
      evidence: {
        sessionId: 'run1',
        urlPath: '/',
        assertions: [],
        consoleErrors: [],
        screenshotId: null,
        truncated: false,
        capturedAt: '',
      },
      commandResults: [],
    });
    const transport = {
      browser: {
        discover: vi.fn(),
        createPreview: vi.fn(),
        startVerification: start,
        getVerification: vi.fn(),
      },
    } as unknown as WorkbenchTransport;

    render(<WorkbenchBrowserVerificationPanel previewId="p1" transport={transport} />);
    await userEvent.click(screen.getByRole('button', { name: '验证当前预览' }));
    expect(start).toHaveBeenCalledWith(
      'p1',
      expect.any(String),
    );
    expect(
      screen.queryByRole('textbox', { name: /脚本|JavaScript|selector/i }),
    ).toBeNull();
  });
});
