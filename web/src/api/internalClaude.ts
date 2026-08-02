/**
 * Internal Claude provider override API — Tauri invoke 封装。
 *
 * Business Logic（为什么需要这个模块）:
 *   设置页 AI tab 选择一个 cc-switch claude provider，专供 cc-partner 内部 headless
 *   Claude 调用（commit/merge/prompt 优化/GitHub 解说/verifier）使用，且不改写 OS 默认
 *   `~/.claude/settings.json`。本 API 只持久化所选 provider **id**（不含凭据）。
 *
 * Code Logic（这个模块做什么）:
 *   封装 get/default/update invoke，组件层只消费类型化 Promise。命令名 snake_case 对齐后端。
 */

import { invoke } from './client';
import type { InternalClaudeConfig } from '@/lib/types';

export interface InternalClaudeConfigUpdate {
  /** 选中的 cc-switch claude provider id；`''` 表示清空（沿用 OS 默认）。始终传字符串。 */
  providerId: string;
}

export const internalClaudeApi = {
  /** 读取内部 Claude provider 覆盖配置。 */
  getConfig: () => invoke<InternalClaudeConfig>('get_internal_claude_config'),

  /** 读取默认配置（providerId=null，供「恢复默认」）。 */
  getDefaultConfig: () =>
    invoke<InternalClaudeConfig>('get_default_internal_claude_config'),

  /**
   * 更新内部 Claude provider 覆盖配置。
   *
   * Code Logic:
   *   始终传字符串：`<id>` 设置，`''` 清空（= 沿用 OS 默认）。后端 trim 空串归一为 None。
   */
  updateConfig: (payload: InternalClaudeConfigUpdate) =>
    invoke<InternalClaudeConfig>(
      'update_internal_claude_config',
      payload as unknown as Record<string, unknown>,
    ),
};
