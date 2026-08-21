/**
 * E2E-SETTINGS-001 — Settings 局部 loader 失败、保存回滚 dirty、深链。
 *
 * Business Logic（为什么需要这个套件）:
 *   10 成功 + 1 非 core loader 失败时其它 tab 仍可用；save reject 保持 dirty；
 *   dependencies/automation deep link 正确落地。
 *
 * Code Logic（这个套件做什么）:
 *   registerSettingsResourceCommands(failGroup=githubTrending)；
 *   改 deviceName → update_config reject → dirty 提示仍在；goto ?tab=。
 */

import { expect, test } from './fixtures';
import {
  installAppLocalStorage,
  makeAppConfig,
  registerAppShellCommands,
  registerSettingsResourceCommands,
} from './support/appBootstrap';

test.describe('E2E-SETTINGS-001 Settings partial failure journey', () => {
  test('non-core loader failure keeps other tabs usable; save reject restores dirty; deep links land', async ({
    page,
    backendHarness,
  }) => {
    await installAppLocalStorage(page);
    const config = makeAppConfig({ deviceName: 'Original Device' });
    registerAppShellCommands(backendHarness, { config });
    // 10 成功 + githubTrending current 失败（非 core）
    registerSettingsResourceCommands(backendHarness, {
      failGroup: 'githubTrending',
      config,
    });

    await page.goto('/settings');
    await expect(page.getByRole('tab', { name: '常规' })).toBeVisible({
      timeout: 15_000,
    });

    // 常规 tab 可用（core 成功）
    const nameField = page.locator('#settings-device-name');
    await expect(nameField).toBeVisible({ timeout: 10_000 });
    await expect(nameField).toHaveValue('Original Device');
    await expect(page.getByText('Prompt 优化快捷键', { exact: true })).toBeVisible();
    await expect(page.getByText(/在工作台终端唤出 Prompt 优化浮层/)).toBeVisible();
    await expect(page.getByText('收藏快捷输入快捷键', { exact: true })).toBeVisible();

    // 其它 tab 仍可切换（dependencies / automation）——非 core github 失败不得整页死
    await page.getByRole('tab', { name: '依赖环境' }).click();
    await expect(page.locator('#settings-panel-dependencies')).toBeVisible({
      timeout: 5_000,
    });
    await page.getByRole('tab', { name: '自动化' }).click();
    await expect(page.locator('#settings-panel-automation')).toBeVisible({
      timeout: 5_000,
    });

    // 回到常规改名并 save reject → dirty 保留
    await page.getByRole('tab', { name: '常规' }).click();
    await expect(nameField).toBeVisible({ timeout: 5_000 });

    await nameField.fill('Dirty Device');
    await expect(page.getByText('有未应用的修改')).toBeVisible({ timeout: 5_000 });

    backendHarness.command('update_config', {
      kind: 'reject',
      error: new Error('save rejected'),
    });
    await page.getByRole('button', { name: '应用配置' }).first().click();
    await expect(page.getByText(/save rejected|保存失败/i)).toBeVisible({
      timeout: 5_000,
    });
    // dirty 未清除
    await expect(page.getByText('有未应用的修改')).toBeVisible();
    await expect(nameField).toHaveValue('Dirty Device');

    // ── deep link dependencies ──
    await page.goto('/settings?tab=dependencies');
    await expect(page.locator('#settings-panel-dependencies')).toBeVisible({
      timeout: 10_000,
    });
    await expect(page.getByRole('tab', { name: '依赖环境' })).toHaveAttribute(
      'aria-selected',
      'true',
    );

    // ── deep link automation ──
    await page.goto('/settings?tab=automation');
    await expect(page.locator('#settings-panel-automation')).toBeVisible({
      timeout: 10_000,
    });
    await expect(page.getByRole('tab', { name: '自动化' })).toHaveAttribute(
      'aria-selected',
      'true',
    );
  });
});
