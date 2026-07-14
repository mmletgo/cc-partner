/**
 * E2E-SCRATCH-001 — 速记本 autosave flush / 失败重试旅程。
 *
 * Business Logic（为什么需要这个套件）:
 *   输入后在 500ms debounce 前导航/卸载必须 flush；保存失败不得伪装成功，并提供重试。
 *
 * Code Logic（这个套件做什么）:
 *   harness 注册 list/get/update_scratchpad；defer update 验证 unmount flush；
 *   reject update 断言 retry 按钮与错误态。
 */

import { expect, test } from './fixtures';
import {
  installAppLocalStorage,
  registerAppShellCommands,
} from './support/appBootstrap';
import type { PlaywrightBackendHarness } from './support/backendHarness';

const PAGE_ID = 'page-1';
const PAGE_TITLE = '工作草稿';

/**
 * Business Logic（为什么需要这个函数）:
 *   速记本摘要/正文 DTO 对齐后端 camelCase。
 *
 * Code Logic（这个函数做什么）:
 *   返回 summary 与 full page，content 可覆盖。
 */
function makeScratchpadPage(content: string) {
  const summary = {
    id: PAGE_ID,
    title: PAGE_TITLE,
    updatedAt: '2026-07-14T00:00:00.000Z',
    preview: content.slice(0, 72),
  };
  const page = {
    id: PAGE_ID,
    title: PAGE_TITLE,
    content,
    createdAt: '2026-07-14T00:00:00.000Z',
    updatedAt: '2026-07-14T00:00:00.000Z',
  };
  return { summary, page };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Scratchpad 旅程需要 list/get 基线与 AppShell 命令。
 *
 * Code Logic（这个函数做什么）:
 *   注册 list_pages + get_page 初始空正文。
 */
function registerScratchpadBase(harness: PlaywrightBackendHarness, content = ''): void {
  registerAppShellCommands(harness);
  const { summary, page } = makeScratchpadPage(content);
  harness.command('list_scratchpad_pages', { kind: 'resolve', value: [summary] });
  harness.command('get_scratchpad_page', { kind: 'resolve', value: page });
}

test.describe('E2E-SCRATCH-001 Scratchpad autosave journey', () => {
  test('type then navigate before 500ms flushes; reject save shows retry', async ({
    page,
    backendHarness,
  }) => {
    await installAppLocalStorage(page);
    registerScratchpadBase(backendHarness, '');

    // 首轮：update 用 defer，确保 unmount flush 会挂起并在 resolve 后落库
    backendHarness.command('update_scratchpad_page_content', {
      kind: 'defer',
      key: 'scratch-save',
    });

    await page.goto('/scratchpad');
    await expect(page.getByRole('heading', { name: '速记本' })).toBeVisible({
      timeout: 15_000,
    });
    const editor = page.getByLabel('速记本内容');
    await expect(editor).toBeVisible({ timeout: 10_000 });

    const draft = 'flush-before-debounce-content';
    await editor.fill(draft);

    // 目标路由命令必须先注册，再导航（否则 unregistered invoke 会在 goto 时炸掉）
    backendHarness.command('list_prompts', { kind: 'resolve', value: [] });
    backendHarness.command('list_tags', { kind: 'resolve', value: [] });
    backendHarness.command('trigger_sync', { kind: 'resolve', value: { synced: 0 } });

    // 在 500ms 前导航离开，触发 unmount flushAll
    await page.goto('/prompts');

    // 等待 flush 调用 update
    await expect
      .poll(
        () =>
          backendHarness
            .calls()
            .filter(
              (call) =>
                call.type === 'invoke' &&
                call.command === 'update_scratchpad_page_content',
            ).length,
        { timeout: 5_000 },
      )
      .toBeGreaterThanOrEqual(1);

    const saveCall = backendHarness
      .calls()
      .find(
        (call) =>
          call.type === 'invoke' && call.command === 'update_scratchpad_page_content',
      );
    expect(saveCall).toMatchObject({
      type: 'invoke',
      command: 'update_scratchpad_page_content',
      args: { pageId: PAGE_ID, content: draft },
    });

    // resolve flush；刷新后内容仍在
    const { summary, page: savedPage } = makeScratchpadPage(draft);
    backendHarness.resolveDeferred('scratch-save', {
      ...savedPage,
      content: draft,
      updatedAt: '2026-07-14T00:05:00.000Z',
    });
    backendHarness.command('list_scratchpad_pages', {
      kind: 'resolve',
      value: [{ ...summary, preview: draft.slice(0, 72) }],
    });
    backendHarness.command('get_scratchpad_page', {
      kind: 'resolve',
      value: { ...savedPage, content: draft },
    });
    // 后续自动保存成功
    backendHarness.command('update_scratchpad_page_content', {
      kind: 'resolve',
      value: { ...savedPage, content: draft },
    });

    await page.goto('/scratchpad');
    await expect(page.getByLabel('速记本内容')).toHaveValue(draft, { timeout: 10_000 });

    // ── 保存 reject → unsaved/retry ──
    backendHarness.command('update_scratchpad_page_content', {
      kind: 'reject',
      error: new Error('disk full'),
    });
    await page.getByLabel('速记本内容').fill(`${draft}-v2`);
    // 等待 debounce 触发失败
    await expect(page.getByRole('button', { name: '重试保存' })).toBeVisible({
      timeout: 5_000,
    });
    await expect(page.getByText(/速记本保存失败：disk full/)).toBeVisible();

    // 重试成功
    backendHarness.command('update_scratchpad_page_content', {
      kind: 'resolve',
      value: {
        ...savedPage,
        content: `${draft}-v2`,
        updatedAt: '2026-07-14T00:06:00.000Z',
      },
    });
    await page.getByRole('button', { name: '重试保存' }).click();
    await expect(page.getByRole('button', { name: '重试保存' })).toHaveCount(0, {
      timeout: 5_000,
    });
    await expect(page.getByText(/速记本保存失败/)).toHaveCount(0);
  });
});
