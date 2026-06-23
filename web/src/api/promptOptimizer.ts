/**
 * Prompt 优化 API - 通过 Tauri invoke 调用 Rust 后端 Claude CLI 优化命令
 *
 * Business Logic（为什么需要这个模块）:
 *   Prompt 优化页需要把用户输入发送给本机 Claude Code CLI，并展示中英文优化结果。
 *
 * Code Logic（这个模块做什么）:
 *   封装 `optimize_prompt` invoke，组件层只消费类型化 Promise，不接触命令名细节。
 */

import { invoke } from './client';
import type { PromptOptimizeResponse } from '@/lib/types';

export const promptOptimizerApi = {
  /** 优化原始 Prompt，返回中文与英文两个版本 */
  optimize: (prompt: string) => invoke<PromptOptimizeResponse>('optimize_prompt', { prompt }),
};
