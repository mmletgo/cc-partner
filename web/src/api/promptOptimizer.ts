/**
 * Prompt 优化 API - 通过 Tauri invoke 调用 Rust 后端 Claude CLI 优化命令
 *
 * Business Logic（为什么需要这个模块）:
 *   Prompt 优化页需要把用户输入发送给本机 Claude Code CLI；Workbench 快捷键小组件需要把优化结果流式写入终端。
 *
 * Code Logic（这个模块做什么）:
 *   封装 `optimize_prompt` 与 Workbench 流式 invoke，组件层只消费类型化 Promise，不接触命令名细节。
 */

import { invoke } from './client';
import type {
  OrchestratorTaskPromptCompletion,
  PromptOptimizeResponse,
  PromptOptimizerFillLanguage,
} from '@/lib/types';

/**
 * Prompt 优化调用选项。
 *
 * Business Logic（为什么需要这个类型）:
 *   Workbench 优化 prompt 时需要把当前项目根目录传给后端，普通优化页则保持无项目上下文。
 *
 * Code Logic（这个类型做什么）:
 *   workingDirectory 为可选绝对目录路径；targetLanguage 供 Workbench 小组件请求单语结果。
 */
export interface PromptOptimizerOptions {
  workingDirectory?: string | null;
  targetLanguage?: PromptOptimizerFillLanguage | null;
}

/**
 * Workbench Prompt 优化流式写入选项。
 *
 * Business Logic（为什么需要这个类型）:
 *   快捷键小组件不展示结果区，而是让后端把优化结果边生成边写入当前活动终端。
 *
 * Code Logic（这个类型做什么）:
 *   sessionId 指定写入的 Workbench terminal session；targetLanguage 始终为设置页选择的单语结果。
 */
export interface PromptOptimizerStreamToTerminalOptions {
  workingDirectory?: string | null;
  targetLanguage: PromptOptimizerFillLanguage;
  sessionId: string;
}

/**
 * Orchestrator 任务 Prompt 完善选项。
 *
 * Business Logic（为什么需要这个类型）:
 *   本机 Workbench 项目可以把项目根目录传给 Claude Code，让 AI 结合项目上下文生成任务字段。
 *
 * Code Logic（这个类型做什么）:
 *   workingDirectory 为空时后端使用 pure/headless 模式；非空时在该目录下运行 Claude CLI。
 */
export interface OrchestratorTaskPromptCompletionOptions {
  workingDirectory?: string | null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   普通 Prompt 优化页需要双语结果，Workbench 小组件只需要设置页选择的单语结果。
 *
 * Code Logic（这个函数做什么）:
 *   归一化 invoke 参数：空目录和空语言转为 null，避免把空字符串传给 Rust 命令层。
 */
export function buildPromptOptimizerInvokeArgs(
  prompt: string,
  options: PromptOptimizerOptions = {},
): Record<string, unknown> {
  return {
    prompt,
    workingDirectory: options.workingDirectory?.trim() || null,
    targetLanguage: options.targetLanguage ?? null,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Workbench 快捷键小组件需要把 prompt 优化结果直接流式写入活动终端。
 *
 * Code Logic（这个函数做什么）:
 *   归一化流式命令参数；工作目录空值转 null，sessionId 原样传给 Rust 命令层做会话校验。
 */
export function buildPromptOptimizerStreamInvokeArgs(
  prompt: string,
  options: PromptOptimizerStreamToTerminalOptions,
): Record<string, unknown> {
  return {
    prompt,
    workingDirectory: options.workingDirectory?.trim() || null,
    targetLanguage: options.targetLanguage,
    sessionId: options.sessionId,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   项目自动化弹窗的 AI 完善按钮需要把简单 Prompt 和可选项目目录传给后端，得到三字段表单草稿。
 *
 * Code Logic（这个函数做什么）:
 *   prompt 和 workingDirectory 都做 trim；空工作目录转 null，避免 Rust 命令层收到空字符串路径。
 */
export function buildOrchestratorTaskPromptCompletionInvokeArgs(
  prompt: string,
  options: OrchestratorTaskPromptCompletionOptions = {},
): Record<string, unknown> {
  return {
    prompt: prompt.trim(),
    workingDirectory: options.workingDirectory?.trim() || null,
  };
}

export const promptOptimizerApi = {
  /** 优化原始 Prompt；不传 targetLanguage 时返回双语，Workbench 可传当前项目根目录和单语设置。 */
  optimize: (prompt: string, options: PromptOptimizerOptions = {}) =>
    invoke<PromptOptimizeResponse>(
      'optimize_prompt',
      buildPromptOptimizerInvokeArgs(prompt, options),
    ),
  /** Workbench 专用：优化结果由后端流式写入指定终端，不返回完整 Prompt 文本。 */
  streamToTerminal: (prompt: string, options: PromptOptimizerStreamToTerminalOptions) =>
    invoke<{ ok: boolean; sessionId: string }>(
      'stream_optimize_prompt_to_workbench_session',
      buildPromptOptimizerStreamInvokeArgs(prompt, options),
    ),
  /** Orchestrator 专用：把简单任务 Prompt 完善为创建任务表单的三个字段。 */
  completeOrchestratorTaskPrompt: (
    prompt: string,
    options: OrchestratorTaskPromptCompletionOptions = {},
  ) =>
    invoke<OrchestratorTaskPromptCompletion>(
      'complete_orchestrator_task_prompt',
      buildOrchestratorTaskPromptCompletionInvokeArgs(prompt, options),
    ),
};
