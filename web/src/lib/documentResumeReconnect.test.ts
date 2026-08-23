import { describe, expect, test } from 'vitest';
import { shouldReconnectOnDocumentResume } from './documentResumeReconnect';

describe('shouldReconnectOnDocumentResume', () => {
  test('reconnects when a live session returns to the visible foreground', () => {
    expect(
      shouldReconnectOnDocumentResume({
        visible: true,
        hasEstablishedSession: true,
        nowMs: 5_000,
        lastReconnectAtMs: null,
      }),
    ).toBe(true);
  });

  test('does not reconnect while the document is hidden', () => {
    expect(
      shouldReconnectOnDocumentResume({
        visible: false,
        hasEstablishedSession: true,
        nowMs: 5_000,
        lastReconnectAtMs: null,
      }),
    ).toBe(false);
  });

  test('does not reconnect before the first session is established', () => {
    expect(
      shouldReconnectOnDocumentResume({
        visible: true,
        hasEstablishedSession: false,
        nowMs: 5_000,
        lastReconnectAtMs: null,
      }),
    ).toBe(false);
  });

  test('reconnects after backgrounding even if no frame was processed yet', () => {
    expect(
      shouldReconnectOnDocumentResume({
        visible: true,
        hasEstablishedSession: false,
        wasBackgrounded: true,
        nowMs: 5_000,
        lastReconnectAtMs: null,
      }),
    ).toBe(true);
  });

  test('always reconnects after a persisted pageshow even if still connecting', () => {
    expect(
      shouldReconnectOnDocumentResume({
        visible: true,
        persistedPageshow: true,
        hasEstablishedSession: false,
        nowMs: 5_000,
        lastReconnectAtMs: null,
      }),
    ).toBe(true);
  });

  test('debounces a second resume within the minimum interval', () => {
    expect(
      shouldReconnectOnDocumentResume({
        visible: true,
        hasEstablishedSession: true,
        nowMs: 1_500,
        lastReconnectAtMs: 1_000,
        minIntervalMs: 1_000,
      }),
    ).toBe(false);
    expect(
      shouldReconnectOnDocumentResume({
        visible: true,
        hasEstablishedSession: true,
        nowMs: 2_000,
        lastReconnectAtMs: 1_000,
        minIntervalMs: 1_000,
      }),
    ).toBe(true);
  });
});
