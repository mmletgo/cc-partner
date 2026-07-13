/**
 * Prompt mutation 纯 reducer 测试
 *
 * Business Logic（为什么需要这个测试）:
 *   create/update/delete 的乐观应用、服务端校准与失败回滚必须保持列表身份与顺序正确。
 *
 * Code Logic（这个测试做什么）:
 *   覆盖 apply/commit/rollback 三种 mutation，以及无关行身份/顺序保持与标签派生。
 */

import { describe, expect, test } from 'vitest';
import type { Prompt } from '@/lib/types';
import {
  applyOptimisticPromptMutation,
  commitPromptMutation,
  deriveTagsFromPrompts,
  promptMutationEntityId,
  rollbackPromptMutation,
  type PromptMutation,
} from './promptMutations';

/**
 * Business Logic（为什么需要这个函数）:
 *   测试夹具需要稳定的 Prompt 行，避免每个用例重复样板字段。
 *
 * Code Logic（这个函数做什么）:
 *   返回可覆盖字段的最小合法 Prompt。
 */
function buildPrompt(overrides: Partial<Prompt> = {}): Prompt {
  return {
    id: 'p-1',
    title: 'Title 1',
    content: 'Content 1',
    tags: ['alpha'],
    updatedAt: '2026-07-13T10:00:00.000Z',
    ...overrides,
  };
}

describe('promptMutations pure reducers', () => {
  const base: Prompt[] = [
    buildPrompt({ id: 'a', title: 'A', content: 'ca', tags: ['t1'] }),
    buildPrompt({ id: 'b', title: 'B', content: 'cb', tags: ['t2'] }),
    buildPrompt({ id: 'c', title: 'C', content: 'cc', tags: ['t1', 't3'] }),
  ];

  test('create applies optimistic row, commits server DTO, and rolls back by removing it', () => {
    const mutation: PromptMutation = {
      kind: 'create',
      optimisticId: 'local-1',
      draft: { title: 'New', content: 'body', tags: ['fresh'] },
    };

    const applied = applyOptimisticPromptMutation(base, mutation, '2026-07-13T12:00:00.000Z');
    expect(applied).toHaveLength(4);
    expect(applied[0]).toMatchObject({
      id: 'local-1',
      title: 'New',
      content: 'body',
      tags: ['fresh'],
      updatedAt: '2026-07-13T12:00:00.000Z',
    });
    // 无关行保持身份与顺序
    expect(applied.slice(1).map((p) => p.id)).toEqual(['a', 'b', 'c']);
    expect(applied[1]).toBe(base[0]);
    expect(applied[2]).toBe(base[1]);
    expect(applied[3]).toBe(base[2]);

    const server = buildPrompt({
      id: 'server-9',
      title: 'New',
      content: 'body',
      tags: ['fresh'],
      updatedAt: '2026-07-13T12:00:01.000Z',
    });
    const committed = commitPromptMutation(applied, mutation, server);
    expect(committed[0]).toBe(server);
    expect(committed.map((p) => p.id)).toEqual(['server-9', 'a', 'b', 'c']);
    expect(committed.slice(1).map((p) => p.id)).toEqual(['a', 'b', 'c']);

    const rolled = rollbackPromptMutation(applied, mutation);
    expect(rolled.map((p) => p.id)).toEqual(['a', 'b', 'c']);
    expect(rolled[0]).toBe(base[0]);
    expect(rolled).toHaveLength(3);
  });

  test('update applies draft, commits canonical server DTO, and rolls back to before', () => {
    const before = base[1];
    const mutation: PromptMutation = {
      kind: 'update',
      id: 'b',
      before,
      draft: { title: 'B2', content: 'cb2', tags: ['t2', 'extra'] },
    };

    const applied = applyOptimisticPromptMutation(base, mutation, '2026-07-13T13:00:00.000Z');
    expect(applied.map((p) => p.id)).toEqual(['a', 'b', 'c']);
    expect(applied[0]).toBe(base[0]);
    expect(applied[2]).toBe(base[2]);
    expect(applied[1]).toMatchObject({
      id: 'b',
      title: 'B2',
      content: 'cb2',
      tags: ['t2', 'extra'],
      updatedAt: '2026-07-13T13:00:00.000Z',
    });

    const server = buildPrompt({
      id: 'b',
      title: 'B2-canonical',
      content: 'cb2-canonical',
      tags: ['t2', 'extra'],
      updatedAt: '2026-07-13T13:00:02.000Z',
      vectorClock: { d1: 3 },
    });
    const committed = commitPromptMutation(applied, mutation, server);
    expect(committed[1]).toBe(server);
    expect(committed[1].title).toBe('B2-canonical');
    expect(committed[0]).toBe(base[0]);
    expect(committed[2]).toBe(base[2]);

    const rolled = rollbackPromptMutation(applied, mutation);
    expect(rolled[1]).toBe(before);
    expect(rolled[1].title).toBe('B');
    expect(rolled.map((p) => p.id)).toEqual(['a', 'b', 'c']);
    expect(rolled[0]).toBe(base[0]);
    expect(rolled[2]).toBe(base[2]);
  });

  test('delete removes row and restores original index on rollback; commit keeps removal', () => {
    const before = base[1];
    const mutation: PromptMutation = {
      kind: 'delete',
      id: 'b',
      before,
      index: 1,
    };

    const applied = applyOptimisticPromptMutation(base, mutation);
    expect(applied.map((p) => p.id)).toEqual(['a', 'c']);
    expect(applied[0]).toBe(base[0]);
    expect(applied[1]).toBe(base[2]);

    const committed = commitPromptMutation(applied, mutation);
    expect(committed.map((p) => p.id)).toEqual(['a', 'c']);

    const rolled = rollbackPromptMutation(applied, mutation);
    expect(rolled.map((p) => p.id)).toEqual(['a', 'b', 'c']);
    expect(rolled[1]).toBe(before);
    expect(rolled[0]).toBe(base[0]);
    expect(rolled[2]).toBe(base[2]);
  });

  test('delete rollback clamps index when list shrank and keeps unrelated identity', () => {
    const mutation: PromptMutation = {
      kind: 'delete',
      id: 'c',
      before: base[2],
      index: 99,
    };
    const applied = applyOptimisticPromptMutation(base, mutation);
    const rolled = rollbackPromptMutation(applied, mutation);
    expect(rolled.map((p) => p.id)).toEqual(['a', 'b', 'c']);
    expect(rolled[2]).toBe(base[2]);
  });

  test('promptMutationEntityId and deriveTagsFromPrompts helpers', () => {
    expect(
      promptMutationEntityId({
        kind: 'create',
        optimisticId: 'local-x',
        draft: { title: 't', content: 'c', tags: [] },
      }),
    ).toBe('local-x');
    expect(
      promptMutationEntityId({
        kind: 'update',
        id: 'u1',
        before: buildPrompt({ id: 'u1' }),
        draft: { title: 't', content: 'c', tags: [] },
      }),
    ).toBe('u1');
    expect(
      promptMutationEntityId({
        kind: 'delete',
        id: 'd1',
        before: buildPrompt({ id: 'd1' }),
        index: 0,
      }),
    ).toBe('d1');

    expect(deriveTagsFromPrompts(base)).toEqual(['t1', 't2', 't3']);
    expect(
      deriveTagsFromPrompts([
        buildPrompt({ id: 'x', tags: [], tag: 'legacy' }),
        buildPrompt({ id: 'y', tags: ['z'] }),
      ]),
    ).toEqual(['legacy', 'z']);
  });
});
