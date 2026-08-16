/**
 * modelContextWindow 单元测试。
 *
 * Business Logic（为什么需要这个测试文件）:
 *   窗口解析必须覆盖 grok-4.6-build / [1M] 与 ccstatusline 200k 回落。
 *
 * Code Logic（这个测试文件做什么）:
 *   断言 hint、表、家族、空输入。
 */
import { describe, expect, it } from 'vitest';
import { parseContextWindowSize, resolveContextWindow } from './modelContextWindow';

describe('parseContextWindowSize', () => {
  it('解析方括号与圆括号 k/M', () => {
    expect(parseContextWindowSize('grok-4.6[1M]')).toBe(1_000_000);
    expect(parseContextWindowSize('claude-sonnet-4-5-20250929[1m]')).toBe(1_000_000);
    expect(parseContextWindowSize('Opus 4.6 (200k)')).toBe(200_000);
  });
});

describe('resolveContextWindow', () => {
  it('已知模型返回精确 token 数', () => {
    expect(resolveContextWindow('claude-sonnet-4-5')).toBe(200_000);
    expect(resolveContextWindow('claude-sonnet-4-5-1m')).toBe(1_000_000);
    expect(resolveContextWindow('claude-opus-4-5-20251101')).toBe(200_000);
    expect(resolveContextWindow('claude-sonnet-4-6-1m-20260101')).toBe(1_000_000);
    expect(resolveContextWindow('gpt-5')).toBe(400_000);
  });

  it('grok-4.6-build / [1M] 显示 1M', () => {
    expect(resolveContextWindow('grok-4.6-build')).toBe(1_000_000);
    expect(resolveContextWindow('grok-4.6[1M]')).toBe(1_000_000);
    expect(resolveContextWindow('  Grok-4.6-Build  ')).toBe(1_000_000);
  });

  it('大小写与首尾空白归一化', () => {
    expect(resolveContextWindow('  Claude-Sonnet-4-5  ')).toBe(200_000);
    expect(resolveContextWindow('GPT-5-CODEX')).toBe(400_000);
  });

  it('未知非空 modelId 回落 200k（对齐 ccstatusline）', () => {
    expect(resolveContextWindow('unknown-model')).toBe(200_000);
    expect(resolveContextWindow('gpt-6')).toBe(200_000);
  });

  it('null / undefined / 空串返回 null', () => {
    expect(resolveContextWindow(null)).toBeNull();
    expect(resolveContextWindow(undefined)).toBeNull();
    expect(resolveContextWindow('')).toBeNull();
    expect(resolveContextWindow('   ')).toBeNull();
  });
});
