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

/**
 * Business Logic（为什么需要这个函数）:
 *   拆分后 API 调用在 controller、部分 UI 守卫在 view，测试需要跨文件扫描。
 *
 * Code Logic（这个函数做什么）:
 *   读取相对本测试文件的 UTF-8 源码。
 */
function readSource(relativePath: string): string {
  return readFileSync(new URL(relativePath, import.meta.url), 'utf8');
}

describe('orchestratorActions', () => {
  test('Orchestrator drawer calls every business action and ships required i18n keys', () => {
    const controller = readSource('./controllers/useOrchestratorController.ts');
    const shell = readSource('./Orchestrator.tsx');
    const outbox = readSource('./views/OrchestratorOutbox.tsx');
    const apiSurface = `${controller}\n${shell}`;
    const zh = JSON.parse(
      readSource('../../i18n/locales/zh/orchestrator.json'),
    ) as Record<string, unknown>;
    const en = JSON.parse(
      readSource('../../i18n/locales/en/orchestrator.json'),
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
      assert(controller.includes(expectedCall), `Orchestrator controller should call ${expectedCall}`);
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
      controller.includes('createAction: OrchestratorCreateAction') ||
        controller.includes('createAction: createAction as ApiOrchestratorCreateAction') ||
        controller.includes('createAction,'),
      'Desktop create dialog should submit an explicit createAction argument',
    );
    assert(
      controller.includes('createAction,'),
      'Desktop create request should pass createAction to orchestratorApi.createTaskView',
    );

    for (const expectedCall of [
      'orchestratorApi.retryRemoteOutbox',
      'orchestratorApi.discardRemoteOutbox',
    ]) {
      assert(controller.includes(expectedCall), `Orchestrator pending outbox should call ${expectedCall}`);
    }
    assert(
      outbox.includes("item.status === 'failed'"),
      'outbox Retry/Discard actions should render only for failed status',
    );
    assert(
      controller.includes("window.confirm(t('orchestrator:pending.discardConfirm'))"),
      'discard should require confirmation in original Automation UI',
    );
    assert(
      apiSurface.includes('focusTaskId') && apiSurface.includes('focusOutboxId'),
      'OrchestratorPanel should accept Attention focus task/outbox props',
    );
    assert(
      controller.includes('onFocusTargetNotFound') ||
        controller.includes('resolveOrchestratorFocusTarget'),
      'Orchestrator should report typed target-not-found for missing Attention deep links',
    );

    const pendingZh = zh.pending as Record<string, unknown>;
    const pendingEn = en.pending as Record<string, unknown>;
    for (const key of ['retry', 'discard', 'discardConfirm']) {
      assert(typeof pendingZh[key] === 'string' && String(pendingZh[key]).length > 0, `zh pending.${key}`);
      assert(typeof pendingEn[key] === 'string' && String(pendingEn[key]).length > 0, `en pending.${key}`);
    }
    const pendingStatusZh = pendingZh.status as Record<string, string>;
    const pendingStatusEn = pendingEn.status as Record<string, string>;
    assert(typeof pendingStatusZh.discarded === 'string', 'zh pending.status.discarded');
    assert(typeof pendingStatusEn.discarded === 'string', 'en pending.status.discarded');
  });
});
