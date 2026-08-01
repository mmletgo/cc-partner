/**
 * E2E-PERM-001 — 权限检查失败/重试、通知不阻断、截图缺权回 Welcome。
 *
 * Business Logic（为什么需要这个套件）:
 *   check_permissions reject 不得永久 loading；retry 恢复；notification 失败不阻断；
 *   screenshot:permission-needed 事件导航 Welcome。
 *
 * Code Logic（这个套件做什么）:
 *   backendHarness 序列化 permission check fail→success；notification reject；
 *   emit screenshot:permission-needed 断言 /welcome。
 */

import { expect, test } from './fixtures';
import {
  installAppLocalStorage,
  makePermissionsStatus,
  registerAppShellCommands,
} from './support/appBootstrap';

test.describe('E2E-PERM-001 Permissions journey', () => {
  test('check reject leaves non-permanent loading; retry recovers; notification does not block', async ({
    page,
    backendHarness,
  }) => {
    await installAppLocalStorage(page, { permissionOnboarded: false });
    registerAppShellCommands(backendHarness, {
      permissions: makePermissionsStatus(false),
      notificationGranted: false,
    });

    // 首轮 check 失败
    backendHarness.command('check_permissions', [
      { kind: 'reject', error: new Error('permission check failed') },
      { kind: 'resolve', value: makePermissionsStatus(false) },
    ]);

    await page.goto('/welcome');

    // 不得永久停在「正在检查权限状态」
    await expect(page.getByText('正在检查权限状态…')).toHaveCount(0, {
      timeout: 10_000,
    });
    await expect(page.getByText(/检查权限失败：permission check failed/)).toBeVisible({
      timeout: 5_000,
    });
    const recheck = page.getByRole('button', { name: '重新检查' });
    await expect(recheck).toBeVisible();

    // 重试成功 → 权限卡出现；通知失败不阻断（卡片列表仍渲染）
    await recheck.click();
    await expect(page.getByText(/屏幕录制/)).toBeVisible({ timeout: 5_000 });
    await expect(page.getByRole('button', { name: '重新检查' })).toBeVisible();
    await expect(page.getByText(/检查权限失败：permission check failed/)).toHaveCount(0);
    await expect(page.getByLabel('权限列表').getByText('输入监控', { exact: true })).toHaveCount(0);
    // 通知权限卡存在且「暂时跳过」仍可用（notification 探测失败不阻断）
    await expect(page.getByLabel('权限列表')).toBeVisible();
    await expect(page.getByLabel('权限列表').getByText('通知', { exact: true })).toBeVisible();
    await expect(page.getByRole('button', { name: '暂时跳过' })).toBeEnabled();
  });

  test('screenshot permission event navigates to Welcome', async ({
    page,
    backendHarness,
  }) => {
    await installAppLocalStorage(page, { permissionOnboarded: true });
    registerAppShellCommands(backendHarness, {
      permissions: makePermissionsStatus(true),
      notificationGranted: true,
    });

    await page.goto('/');
    await expect(page.getByRole('navigation').or(page.locator('aside')).first()).toBeVisible({
      timeout: 15_000,
    });

    // 后端截图缺权事件
    backendHarness.emit('screenshot:permission-needed', {
      reason: 'screen_capture_denied',
    });

    await expect(page).toHaveURL(/\/welcome/, { timeout: 5_000 });
    await expect(page.getByRole('heading', { name: /欢迎|Welcome|权限/ }).or(
      page.getByText(/正在检查权限|权限/),
    ).first()).toBeVisible({ timeout: 10_000 });
  });
});
