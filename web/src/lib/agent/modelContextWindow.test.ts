/**
 * modelContextWindow 单元测试。
 *
 * Business Logic（为什么需要这个测试文件）:
 *   context window 命中 / 未命中 / null 输入是用户可见的百分比基石；
 *   必须防止前缀归一化或大小写处理回归。
 *
 * Code Logic（这个测试文件做什么）:
 *   直接断言 resolveContextWindow 的已知模型、大小写归一、前缀裁剪、null 路径。
 */
import { describe, expect, it } from 'vitest';
import { resolveContextWindow } from './modelContextWindow';

describe('resolveContextWindow', () => {
  it('已知模型返回精确 token 数', () => {
    expect(resolveContextWindow('claude-sonnet-4-5')).toBe(200_000);
    expect(resolveContextWindow('claude-sonnet-4-5-1m')).toBe(1_000_000);
    expect(resolveContextWindow('claude-opus-4')).toBe(200_000);
    expect(resolveContextWindow('gpt-5')).toBe(400_000);
  });

  it('大小写与首尾空白归一化', () => {
    expect(resolveContextWindow('  Claude-Sonnet-4-5  ')).toBe(200_000);
    expect(resolveContextWindow('GPT-5-CODEX')).toBe(400_000);
  });

  it('未知模型返回 null（禁止假装 200K）', () => {
    expect(resolveContextWindow('unknown-model')).toBeNull();
    expect(resolveContextWindow('gpt-6')).toBeNull();
  });

  it('null / undefined / 空串返回 null', () => {
    expect(resolveContextWindow(null)).toBeNull();
    expect(resolveContextWindow(undefined)).toBeNull();
    expect(resolveContextWindow('')).toBeNull();
    expect(resolveContextWindow('   ')).toBeNull();
  });
});