import { describe, test } from 'vitest';
import { readFileSync } from 'node:fs';

/**
 * Business Logic（为什么需要这个函数）:
 *   Orchestrator drawer 的显式业务动作是用户主要操作入口，静态测试需要在按钮调用或文案遗漏时快速失败。
 *
 * Code Logic（这个函数做什么）:
 *   condition 为 false 时抛出 Error，让 tsx 测试进程以非零状态退出。
 */
function assert(condition: boolean, message: string): void {
  if (!condition) throw new Error(message);
}

describe('orchestratorActions', () => {
  test('Orchestrator drawer calls every business action and ships required i18n keys', () => {
    const source = readFileSync(new URL('./Orchestrator.tsx', import.meta.url), 'utf8');
    const zh = JSON.parse(
      readFileSync(new URL('../../i18n/locales/zh/orchestrator.json', import.meta.url), 'utf8'),
    ) as Record<string, unknown>;
    const en = JSON.parse(
      readFileSync(new URL('../../i18n/locales/en/orchestrator.json', import.meta.url), 'utf8'),
    ) as Record<string, unknown>;

    const detailZh = zh.detail as Record<string, string>;
    const detailEn = en.detail as Record<string, string>;
    const createZh = zh.create as Record<string, string>;
    const createEn = en.create as Record<string, string>;
    const errorsZh = zh.errors as Record<string, string>;
    const errorsEn = en.errors as Record<string, string>;

    for (const expectedCall of [
      'orchestratorApi.startTaskView',
      'orchestratorApi.retryTaskView',
      'orchestratorApi.requestReworkTaskView',
      'orchestratorApi.deliverReviewedTaskView',
      'orchestratorApi.cancelTaskView',
      'orchestratorApi.refreshProject',
    ]) {
      assert(source.includes(expectedCall), `Orchestrator drawer should call ${expectedCall}`);
    }

    for (const key of ['start', 'retry', 'requestRework', 'deliver', 'cancel', 'openWorkbench']) {
      assert(typeof detailZh[key] === 'string' && detailZh[key].length > 0, `zh detail.${key} is required`);
      assert(typeof detailEn[key] === 'string' && detailEn[key].length > 0, `en detail.${key} is required`);
    }

    for (const key of ['start', 'retry', 'requestRework', 'deliver', 'cancel', 'refresh']) {
      assert(typeof errorsZh[key] === 'string' && errorsZh[key].length > 0, `zh errors.${key} is required`);
      assert(typeof errorsEn[key] === 'string' && errorsEn[key].length > 0, `en errors.${key} is required`);
    }

    for (const key of ['createBacklog', 'createTodo', 'createStart']) {
      assert(typeof createZh[key] === 'string' && createZh[key].length > 0, `zh create.${key} is required`);
      assert(typeof createEn[key] === 'string' && createEn[key].length > 0, `en create.${key} is required`);
    }

    assert(
      source.includes('createAction: OrchestratorCreateAction'),
      'Desktop create dialog should submit an explicit createAction argument',
    );
    assert(
      source.includes('createAction,'),
      'Desktop create request should pass createAction to orchestratorApi.createTaskView',
    );
  });
});
