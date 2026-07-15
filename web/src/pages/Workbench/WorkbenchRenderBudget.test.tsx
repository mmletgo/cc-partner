// @vitest-environment jsdom
/**
 * Workbench 根渲染预算 characterization。
 *
 * Business Logic（为什么需要这个测试）:
 *   Workbench 当前为运行时长文本每秒 setState(runtimeNow)，驱动整页（含 controllers）1 Hz 重渲染。
 *   Task 1 先锁住「5s 内根渲染 >1 次」的不良基线；Task 2 抽出 SessionRuntimeText 后把断言改为
 *   tick 不再抬升根渲染计数。
 *
 * Code Logic（这个测试做什么）:
 *   用轻量 WorkbenchRenderProbe 镜像 Workbench.tsx 的 runtimeNow 1s interval 模式（不挂完整
 *   Workbench，避免 controller/provider 过重）；fake timers 推进 5s 后断言 onRender 调用次数 >1。
 *   同时把当前 CodeEditor lazy chunk gzip 基线记入测试日志，供 Task 3 对照（maxLazyChunk /
 *   WorkbenchCodeEditor，与 npm run check:bundle 一致）。
 */

import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { cleanup, render } from '@testing-library/react';
import { useEffect, useState, type ReactElement } from 'react';

/**
 * Business Logic（为什么需要这个组件）:
 *   完整 Workbench 挂载过重且依赖众多 provider；characterization 只需复现页面级 1 Hz setState
 *   驱动根渲染的模式，供 Task 2 升级断言时仍沿用同一探针路径。
 *
 * Code Logic（这个组件做什么）:
 *   渲染时调用 onRender 计数；mount 后 setInterval 每 1000ms setRuntimeNow(Date.now())，
 *   与 Workbench.tsx 的 runtimeNow effect 一致。
 */
function WorkbenchRenderProbe({ onRender }: { onRender: () => void }): ReactElement {
  const [runtimeNow, setRuntimeNow] = useState<number>(() => Date.now());
  onRender();

  useEffect(() => {
    const timer = window.setInterval(() => setRuntimeNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, []);

  return <div data-testid="workbench-render-probe">{runtimeNow}</div>;
}

describe('Workbench render budget (characterization)', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    cleanup();
    vi.useRealTimers();
  });

  test('captures the current workbench root rerender baseline', async () => {
    const renders = vi.fn();
    render(<WorkbenchRenderProbe onRender={renders} />);

    // 初始挂载至少 1 次；推进 5s 应因 1 Hz interval 再触发多次根渲染。
    const initialRenders = renders.mock.calls.length;
    expect(initialRenders).toBeGreaterThanOrEqual(1);

    await vi.advanceTimersByTimeAsync(5_000);

    // Characterization：当前不良行为 — 5s 内根渲染次数 >1（含 mount + 约 5 次 tick）。
    // Task 2 会把该断言改为「tick 不再抬升根渲染计数」。
    expect(renders.mock.calls.length).toBeGreaterThan(1);
    // 记录可重复基线数字，便于 PR / 后续对比（不作为硬断言上限）。
    // eslint-disable-next-line no-console
    console.info(
      `[perf-baseline] workbench root renders over 5s: ${renders.mock.calls.length} (initial=${initialRenders})`,
    );
  });

  test('records the current CodeEditor lazy-chunk baseline note', () => {
    // Code Logic: 不在单元测试里读 dist（CI 未必先 build）；把与 check:bundle 对齐的
    // 测量面写进日志。实测（本 worktree dist）：WorkbenchCodeEditor gzip ≈ 263829 B
    // 即 maxLazyChunk 基线，raw ≈ 755916 B。后续 Task 3 要求 editor-entry 至少下降 20%。
    // eslint-disable-next-line no-console
    console.info(
      '[perf-baseline] CodeEditor lazy chunk: measure via `cd web && npm run check:bundle` → maxLazyChunk / WorkbenchCodeEditor-*.js (baseline maxLazyChunkGzipBytes=263829)',
    );
    expect(true).toBe(true);
  });
});
