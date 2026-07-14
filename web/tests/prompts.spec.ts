/**
 * E2E-PROMPTS-001 — Prompt 库 create/update/delete 乐观回滚与成功提交。
 *
 * Business Logic（为什么需要这个套件）:
 *   create/update/delete 各自 reject 一次必须恢复原顺序/数据并显示 retry；
 *   成功时本地 temp 行由服务端 DTO 替换。
 *
 * Code Logic（这个套件做什么）:
 *   backendHarness 序列化 fail-once 再 success；断言 mutation error banner 与列表卡片 id。
 */

import { expect, test } from './fixtures';
import {
  installAppLocalStorage,
  registerAppShellCommands,
} from './support/appBootstrap';
import type { PlaywrightBackendHarness } from './support/backendHarness';

type PromptDto = {
  id: string;
  title: string;
  content: string;
  tags: string[];
  updatedAt: string;
};

const BASE: PromptDto = {
  id: 'prompt-1',
  title: '已有标题',
  content: '已有内容',
  tags: ['base'],
  updatedAt: '2026-07-14T00:00:00.000Z',
};

/**
 * Business Logic（为什么需要这个函数）:
 *   Prompts 页需要 list/tags/sync 与 CRUD 命令基线。
 *
 * Code Logic（这个函数做什么）:
 *   注册 AppShell + list_prompts/list_tags/trigger_sync。
 */
function registerPromptBase(harness: PlaywrightBackendHarness, prompts: PromptDto[]): void {
  registerAppShellCommands(harness);
  harness.command('list_prompts', { kind: 'resolve', value: prompts });
  harness.command('list_tags', {
    kind: 'resolve',
    value: Array.from(new Set(prompts.flatMap((p) => p.tags))).sort(),
  });
  harness.command('trigger_sync', { kind: 'resolve', value: { synced: 0 } });
}

test.describe('E2E-PROMPTS-001 Prompts mutation journey', () => {
  test('create/update/delete reject restores data; success replaces temp row', async ({
    page,
    backendHarness,
  }) => {
    await installAppLocalStorage(page);
    registerPromptBase(backendHarness, [BASE]);

    await page.goto('/prompts');
    await expect(page.getByRole('heading', { name: 'Prompt 库' })).toBeVisible({
      timeout: 15_000,
    });
    await expect(page.getByText('已有标题')).toBeVisible();
    await expect(page.getByTestId('prompt-card-prompt-1')).toBeVisible();

    // ── create reject once → 恢复草稿 + 列表无 temp ──
    backendHarness.command('create_prompt', [
      { kind: 'reject', error: new Error('create rejected') },
      {
        kind: 'resolve',
        value: {
          id: 'server-1',
          title: '新建标题',
          content: '新建内容',
          tags: [],
          updatedAt: '2026-07-14T00:10:00.000Z',
        } satisfies PromptDto,
      },
    ]);

    await page.getByRole('button', { name: '新建' }).click();
    await page.getByLabel('Prompt 标题').fill('新建标题');
    await page.getByLabel('Prompt 内容').fill('新建内容');
    await page.getByRole('button', { name: '保存' }).click();

    await expect(page.getByTestId('prompt-mutation-error')).toBeVisible({ timeout: 5_000 });
    await expect(page.getByText(/创建失败：create rejected/)).toBeVisible();
    await expect(page.getByLabel('Prompt 标题')).toHaveValue('新建标题');
    await expect(page.getByLabel('Prompt 内容')).toHaveValue('新建内容');
    await expect(page.getByRole('heading', { name: '新建标题' })).toHaveCount(0);
    // 原列表顺序/数据保留
    await expect(page.getByTestId('prompt-card-prompt-1')).toBeVisible();

    // 可见 retry：再次保存成功 → server DTO 替换 temp
    await page.getByRole('button', { name: '保存' }).click();
    await expect(page.getByTestId('prompt-mutation-error')).toHaveCount(0, {
      timeout: 5_000,
    });
    await expect(page.getByTestId('prompt-card-server-1')).toBeVisible({ timeout: 5_000 });
    await expect(page.getByRole('heading', { name: '新建标题' })).toBeVisible();
    // temp 行不应残留
    await expect(page.locator('[data-testid^="prompt-card-temp"]')).toHaveCount(0);

    // ── update reject once → 列表回 before，草稿保留 ──
    backendHarness.command('list_prompts', {
      kind: 'resolve',
      value: [
        {
          id: 'server-1',
          title: '新建标题',
          content: '新建内容',
          tags: [],
          updatedAt: '2026-07-14T00:10:00.000Z',
        },
        BASE,
      ],
    });
    backendHarness.command('update_prompt', [
      { kind: 'reject', error: new Error('update rejected') },
      {
        kind: 'resolve',
        value: {
          id: 'server-1',
          title: '改后标题',
          content: '改后内容',
          tags: [],
          updatedAt: '2026-07-14T00:11:00.000Z',
        } satisfies PromptDto,
      },
    ]);

    await page.getByTestId('prompt-card-server-1').getByRole('button', { name: '编辑' }).click();
    await page.getByLabel('Prompt 标题').fill('改后标题');
    await page.getByLabel('Prompt 内容').fill('改后内容');
    await page.getByRole('button', { name: '保存' }).click();

    await expect(page.getByTestId('prompt-mutation-error')).toBeVisible({ timeout: 5_000 });
    await expect(page.getByText(/更新失败：update rejected/)).toBeVisible();
    await expect(page.getByLabel('Prompt 标题')).toHaveValue('改后标题');
    await expect(page.getByLabel('Prompt 内容')).toHaveValue('改后内容');

    // retry success
    await page.getByRole('button', { name: '保存' }).click();
    await expect(page.getByTestId('prompt-mutation-error')).toHaveCount(0, {
      timeout: 5_000,
    });
    await expect(page.getByText('改后标题')).toBeVisible();

    // ── delete reject once → 条目恢复 ──
    backendHarness.command('delete_prompt', [
      { kind: 'reject', error: new Error('delete rejected') },
      { kind: 'resolve', value: undefined },
    ]);

    await page.getByTestId('prompt-card-prompt-1').getByRole('button', { name: '删除' }).click();
    await expect(page.getByRole('heading', { name: '删除 Prompt？' })).toBeVisible();
    await page.getByRole('button', { name: '删除' }).last().click();

    await expect(page.getByTestId('prompt-mutation-error')).toBeVisible({ timeout: 5_000 });
    await expect(page.getByText(/删除失败：delete rejected/)).toBeVisible();
    await expect(page.getByTestId('prompt-card-prompt-1')).toBeVisible();
    await expect(page.getByText('已有标题')).toBeVisible();

    // retry delete success
    await page.getByTestId('prompt-card-prompt-1').getByRole('button', { name: '删除' }).click();
    await page.getByRole('button', { name: '删除' }).last().click();
    await expect(page.getByTestId('prompt-card-prompt-1')).toHaveCount(0, {
      timeout: 5_000,
    });
  });
});
