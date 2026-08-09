/**
 * Prompt 列表乐观 mutation 纯函数
 *
 * Business Logic（为什么需要这个模块）:
 *   Prompt 新建 / 更新 / 删除必须允许乐观展示，但 API 失败时不能保留伪成功状态。
 *   纯 reducer 把 apply / commit / rollback 从页面副作用中剥离，保证可测且可重放。
 *
 * Code Logic（这个模块做什么）:
 *   以 Prompt[] + PromptMutation 为输入，返回新列表；commit 用服务端 DTO 校准；
 *   rollback 恢复快照；另提供标签派生与实体 id 解析。
 */

import type { Prompt } from '@/lib/types';

/** 编辑/新建草稿（与 UI 表单字段对齐，不含服务端元数据） */
export interface PromptDraft {
  title: string;
  content: string;
  tags: string[];
}

/**
 * 一次 Prompt mutation 的完整快照。
 *
 * Business Logic（为什么需要）:
 *   失败回滚与原地重试都依赖保存的 payload，不能只靠当前 UI 状态。
 *
 * Code Logic（字段说明）:
 *   create 用 optimisticId 占位；update/delete 保存 before；delete 另存原 index。
 */
export type PromptMutation =
  | { kind: 'create'; optimisticId: string; draft: PromptDraft }
  | { kind: 'update'; id: string; before: Prompt; draft: PromptDraft }
  | { kind: 'delete'; id: string; before: Prompt; index: number };

/**
 * Business Logic（为什么需要这个函数）:
 *   页面需要在 mutation 进行中禁用同一实体的冲突动作（再次编辑/删除）。
 *
 * Code Logic（这个函数做什么）:
 *   从 mutation union 取出实体 id：create 用 optimisticId，其余用 id。
 */
export function promptMutationEntityId(mutation: PromptMutation): string {
  switch (mutation.kind) {
    case 'create':
      return mutation.optimisticId;
    case 'update':
    case 'delete':
      return mutation.id;
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   用户保存后应立刻看到结果，不能等网络往返才更新列表。
 *
 * Code Logic（这个函数做什么）:
 *   create 前置乐观行；update 覆盖匹配行字段；delete 移除匹配行。返回新数组，不改原数组。
 */
export function applyOptimisticPromptMutation(
  prompts: readonly Prompt[],
  mutation: PromptMutation,
  nowIso: string = new Date().toISOString(),
): Prompt[] {
  switch (mutation.kind) {
    case 'create': {
      const optimistic: Prompt = {
        id: mutation.optimisticId,
        title: mutation.draft.title,
        content: mutation.draft.content,
        tags: [...mutation.draft.tags],
        favorite: false,
        updatedAt: nowIso,
      };
      return [optimistic, ...prompts];
    }
    case 'update': {
      return prompts.map((prompt) =>
        prompt.id === mutation.id
          ? {
              ...prompt,
              title: mutation.draft.title,
              content: mutation.draft.content,
              tags: [...mutation.draft.tags],
              updatedAt: nowIso,
            }
          : prompt,
      );
    }
    case 'delete': {
      return prompts.filter((prompt) => prompt.id !== mutation.id);
    }
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   API 成功后列表必须以服务端权威 DTO 为准，避免乐观字段与后端分叉。
 *
 * Code Logic（这个函数做什么）:
 *   create 用 server 替换 optimisticId 行；update 用 server 替换 id 行；
 *   delete 确保目标 id 不存在。server 缺省时 create 移除乐观行。
 */
export function commitPromptMutation(
  prompts: readonly Prompt[],
  mutation: PromptMutation,
  server?: Prompt,
): Prompt[] {
  switch (mutation.kind) {
    case 'create': {
      if (!server) {
        return prompts.filter((prompt) => prompt.id !== mutation.optimisticId);
      }
      let replaced = false;
      const next = prompts.map((prompt) => {
        if (prompt.id !== mutation.optimisticId) return prompt;
        replaced = true;
        return server;
      });
      return replaced ? next : [server, ...prompts];
    }
    case 'update': {
      if (!server) {
        return [...prompts];
      }
      let replaced = false;
      const next = prompts.map((prompt) => {
        if (prompt.id !== mutation.id) return prompt;
        replaced = true;
        return server;
      });
      return replaced ? next : [...prompts];
    }
    case 'delete': {
      return prompts.filter((prompt) => prompt.id !== mutation.id);
    }
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   API 失败时必须撤销乐观变更，不能留下“看起来成功”的伪记录。
 *
 * Code Logic（这个函数做什么）:
 *   create 删除 optimistic 行；update 恢复 before；delete 按原 index 插回 before。
 */
export function rollbackPromptMutation(
  prompts: readonly Prompt[],
  mutation: PromptMutation,
): Prompt[] {
  switch (mutation.kind) {
    case 'create': {
      return prompts.filter((prompt) => prompt.id !== mutation.optimisticId);
    }
    case 'update': {
      let restored = false;
      const next = prompts.map((prompt) => {
        if (prompt.id !== mutation.id) return prompt;
        restored = true;
        return mutation.before;
      });
      return restored ? next : [...prompts, mutation.before];
    }
    case 'delete': {
      if (prompts.some((prompt) => prompt.id === mutation.before.id)) {
        return [...prompts];
      }
      const next = [...prompts];
      const index = Math.max(0, Math.min(mutation.index, next.length));
      next.splice(index, 0, mutation.before);
      return next;
    }
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   标签 chips 必须与当前列表一致，避免 tags API 与乐观列表短暂分叉。
 *
 * Code Logic（这个函数做什么）:
 *   从 prompts 收集 tags（兼容 legacy tag），去重后按字典序返回。
 */
export function deriveTagsFromPrompts(prompts: readonly Prompt[]): string[] {
  const tags = new Set<string>();
  for (const prompt of prompts) {
    const promptTags =
      prompt.tags && prompt.tags.length > 0
        ? prompt.tags
        : prompt.tag
          ? [prompt.tag]
          : [];
    for (const tag of promptTags) {
      if (tag) tags.add(tag);
    }
  }
  return Array.from(tags).sort((a, b) => a.localeCompare(b));
}

/**
 * Business Logic（为什么需要这个函数）:
 *   收藏是高频一键操作，用户点击星标后必须立即看到翻转结果，不能等网络往返；
 *   失败时由调用方再 flip 回滚。收藏独立于 create/update/delete mutation 事务，
 *   不应走 runMutation 的 pending 门闩（避免与正在进行的编辑冲突）。
 *
 * Code Logic（这个函数做什么）:
 *   返回新数组，命中 id 的行翻转 favorite 字段，其余行原样保留；不修改其他字段。
 *   未命中 id 时返回原数组引用（无变化）。
 *
 * @param prompts 当前列表（只读）
 * @param id 待翻转收藏的 Prompt id
 * @returns 翻转后的新列表；未命中时返回原引用
 */
export function applyFavoriteToggle(
  prompts: readonly Prompt[],
  id: string,
): Prompt[] {
  const target = prompts.find((p) => p.id === id);
  if (!target) return prompts as Prompt[];
  return prompts.map((prompt) =>
    prompt.id === id ? { ...prompt, favorite: !prompt.favorite } : prompt,
  );
}
