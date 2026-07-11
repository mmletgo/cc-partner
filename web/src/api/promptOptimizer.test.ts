import { describe, test } from 'vitest';
import {
  buildOrchestratorTaskPromptCompletionInvokeArgs,
  buildPromptOptimizerInvokeArgs,
  buildPromptOptimizerStreamInvokeArgs,
} from './promptOptimizer';

/**
 * Business Logic（为什么需要这个函数）:
 *   Prompt 优化 API 测试需要清晰断言参数是否按设置语种传给后端。
 *
 * Code Logic（这个函数做什么）:
 *   condition 为 false 时抛错，让测试用例失败。
 */
function assert(condition: boolean, message: string): void {
  if (!condition) throw new Error(message);
}

describe('promptOptimizer', () => {
  test('builds optimize, stream, and orchestrator completion invoke args', () => {
    const args = buildPromptOptimizerInvokeArgs('修复问题', {
      workingDirectory: '/Users/hans/project/Pando',
      targetLanguage: 'zh',
    });

    assert(
      JSON.stringify(args) ===
        JSON.stringify({
          prompt: '修复问题',
          workingDirectory: '/Users/hans/project/Pando',
          targetLanguage: 'zh',
        }),
      'optimize should pass targetLanguage to backend',
    );

    const streamArgs = buildPromptOptimizerStreamInvokeArgs('优化并填入终端', {
      workingDirectory: ' /Users/hans/project/Pando ',
      targetLanguage: 'en',
      sessionId: 'session-1',
    });

    assert(
      JSON.stringify(streamArgs) ===
        JSON.stringify({
          prompt: '优化并填入终端',
          workingDirectory: '/Users/hans/project/Pando',
          targetLanguage: 'en',
          sessionId: 'session-1',
        }),
      'streamToTerminal should pass sessionId and selected targetLanguage to backend',
    );

    const orchestratorCompletionArgs = buildOrchestratorTaskPromptCompletionInvokeArgs(
      '修复自动化任务创建体验',
      {
        workingDirectory: ' /Users/hans/project/Pando ',
      },
    );

    assert(
      JSON.stringify(orchestratorCompletionArgs) ===
        JSON.stringify({
          prompt: '修复自动化任务创建体验',
          workingDirectory: '/Users/hans/project/Pando',
        }),
      'orchestrator task prompt completion should normalize prompt and optional working directory',
    );
  });
});
