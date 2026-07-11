import { describe, test } from 'vitest';
import type { OrchestratorAutomationConfig } from '@/api/orchestratorConfig';
import {
  automationConfigToForm,
  automationFormToPatch,
  clampAutomationMaxConcurrentTasks,
  commandsToTextarea,
  isAutomationFormDirty,
  PENDING_AUTOMATION_SETTINGS_FORM,
  textareaToCommandsText,
} from './automationSettingsState';

/**
 * Business Logic（为什么需要）:
 *   Settings 自动化 tab 的表单 helper 需要无框架测试，确保配置数组、textarea 文本和保存 patch
 *   之间的转换稳定，避免 UI 接入后把验证命令或布尔交付开关写错。
 *
 * Code Logic（做什么）:
 *   使用 JSON 序列化比较实际值与期望值；不一致时抛错让 tsx 进程以非零状态退出。
 */
function assertDeepEqual(actual: unknown, expected: unknown): void {
  const actualJson = JSON.stringify(actual);
  const expectedJson = JSON.stringify(expected);
  if (actualJson !== expectedJson) {
    throw new Error(`Expected ${expectedJson}, got ${actualJson}`);
  }
}

/**
 * Business Logic（为什么需要）:
 *   pending form 是共享常量，转换 null 配置时必须返回新对象，防止调用方误改常量污染后续测试或页面状态。
 *
 * Code Logic（做什么）:
 *   断言两个对象不是同一个引用；相同则抛错。
 */
function assertNotSameRef(actual: unknown, expected: unknown): void {
  if (actual === expected) {
    throw new Error('Expected distinct object references, got the same reference');
  }
}

/**
 * Business Logic（为什么需要）:
 *   多个测试都需要一份完整 Orchestrator 自动化配置，集中构造可减少无关字段重复。
 *
 * Code Logic（做什么）:
 *   返回符合前端 API DTO 的配置对象，允许局部覆盖字段。
 */
function configFixture(
  partial: Partial<OrchestratorAutomationConfig> = {},
): OrchestratorAutomationConfig {
  return {
    enabled: true,
    maxConcurrentTasks: 2,
    verificationCommands: ['cargo test', 'npm run lint'],
    autoCommit: true,
    autoPushTaskBranch: false,
    autoMergeToMain: true,
    autoPushMain: false,
    ...partial,
  };
}

describe('automationSettingsState', () => {
  test('converts config <-> form <-> patch and clamps concurrency', () => {
    assertDeepEqual(commandsToTextarea(['cargo test', 'npm run lint']), 'cargo test\nnpm run lint');
    assertDeepEqual(textareaToCommandsText('cargo test\r\n\nnpm run lint'), 'cargo test\n\nnpm run lint');
    assertDeepEqual(clampAutomationMaxConcurrentTasks(Number.NaN), 1);
    assertDeepEqual(clampAutomationMaxConcurrentTasks(0), 1);
    assertDeepEqual(clampAutomationMaxConcurrentTasks(2.8), 2);
    assertDeepEqual(clampAutomationMaxConcurrentTasks(9), 8);

    const pending = automationConfigToForm(null);
    assertDeepEqual(pending, PENDING_AUTOMATION_SETTINGS_FORM);
    assertNotSameRef(pending, PENDING_AUTOMATION_SETTINGS_FORM);

    const loaded = automationConfigToForm(configFixture());
    assertDeepEqual(loaded, {
      enabled: true,
      maxConcurrentTasks: 2,
      verificationCommandsText: 'cargo test\nnpm run lint',
      autoCommit: true,
      autoPushTaskBranch: false,
      autoMergeToMain: true,
      autoPushMain: false,
    });

    assertDeepEqual(automationConfigToForm(configFixture({ maxConcurrentTasks: 99 })).maxConcurrentTasks, 8);

    assertDeepEqual(automationFormToPatch(loaded), {
      enabled: true,
      maxConcurrentTasks: 2,
      verificationCommands: 'cargo test\nnpm run lint',
      autoCommit: true,
      autoPushTaskBranch: false,
      autoMergeToMain: true,
      autoPushMain: false,
    });

    assertDeepEqual(
      automationFormToPatch({
        ...loaded,
        maxConcurrentTasks: -4,
      }).maxConcurrentTasks,
      1,
    );

    assertDeepEqual(isAutomationFormDirty(loaded, loaded), false);
    assertDeepEqual(
      isAutomationFormDirty(
        {
          ...loaded,
          autoPushMain: true,
        },
        loaded,
      ),
      true,
    );
  });
});
