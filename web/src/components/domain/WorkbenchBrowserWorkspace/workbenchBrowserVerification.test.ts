import { describe, expect, it } from 'vitest';
import {
  buildDefaultVerificationStart,
  isBrowserVerificationTerminal,
  screenshotDataUrl,
  summarizeVerification,
} from './workbenchBrowserVerification';
import type { BrowserVerificationRun } from '@/lib/types';

describe('workbenchBrowserVerification', () => {
  it('default start only carries previewId and requestId', () => {
    const req = buildDefaultVerificationStart('p1', 'r1');
    expect(req).toEqual({ previewId: 'p1', requestId: 'r1' });
    expect(JSON.stringify(req)).not.toMatch(/targetUrl|selector|javascript|eval/i);
  });

  it('terminal states', () => {
    expect(isBrowserVerificationTerminal('succeeded')).toBe(true);
    expect(isBrowserVerificationTerminal('running')).toBe(false);
  });

  it('screenshot data url', () => {
    expect(screenshotDataUrl('abc')).toBe('data:image/png;base64,abc');
    expect(screenshotDataUrl(null)).toBeNull();
  });

  it('summarize evidence without fill values', () => {
    const run: BrowserVerificationRun = {
      session: {
        id: 's',
        projectId: 'p',
        previewId: 'pv',
        ownerInstanceId: 'o',
        state: 'succeeded',
        createdAt: '',
        lastActivityAt: '',
        expiresAt: '',
      },
      evidence: {
        sessionId: 's',
        urlPath: '/',
        assertions: [{ name: 'a', passed: false }],
        consoleErrors: [{ sequence: 1, level: 'error', text: 'x', timestampMs: 0 }],
        screenshotId: 'shot',
        truncated: false,
        capturedAt: '',
      },
    };
    const summary = summarizeVerification(run);
    expect(summary.consoleErrors).toBe(1);
    expect(summary.assertionFailed).toBe(1);
    expect(JSON.stringify(summary)).not.toContain('value');
  });
});
