import { describe, test, expect } from 'vitest';
import { readFileSync } from 'node:fs';

/**
 * Business Logic（为什么需要这个测试文件）:
 *   S4 Task 10 把 Settings.tsx 巨型页面拆为 useSettingsController + pure panels；
 *   必须静态锁住 ownership，防止 panel 回退直连 transport，或 controller 再吞 tab JSX。
 *
 * Code Logic（这个测试文件做什么）:
 *   读取 Settings 目录源码，断言 panels 无 @/api / Api. / invoke 运输面，
 *   controller 无 activeTab 大块 panel 树，Settings 壳层消费 controller 与全部 extracted panels。
 */

const dir = new URL('./', import.meta.url);

/**
 * 读取 Settings 目录下相对路径源码
 *
 * Business Logic（为什么需要这个函数）:
 *   ownership 测试要对多个文件做静态扫描，需要统一 UTF-8 读取。
 *
 * Code Logic（这个函数做什么）:
 *   基于 import.meta.url 解析相对路径并 readFileSync。
 *
 * @param relativePath 相对 Settings 目录的文件名
 * @returns 文件全文
 */
function readSettingsSource(relativePath: string): string {
  return readFileSync(new URL(relativePath, dir), 'utf8');
}

const generalPanel = readSettingsSource('SettingsGeneralPanel.tsx');
const syncPanel = readSettingsSource('SettingsSyncPanel.tsx');
const depsPanel = readSettingsSource('SettingsDependenciesPanel.tsx');
const aiPanel = readSettingsSource('SettingsAiPanel.tsx');
const aboutPanel = readSettingsSource('SettingsAboutPanel.tsx');
const controller = readSettingsSource('useSettingsController.ts');
const settingsShell = readSettingsSource('Settings.tsx');

const PANEL_SOURCES: Array<{ name: string; source: string }> = [
  { name: 'SettingsGeneralPanel.tsx', source: generalPanel },
  { name: 'SettingsSyncPanel.tsx', source: syncPanel },
  { name: 'SettingsDependenciesPanel.tsx', source: depsPanel },
  { name: 'SettingsAiPanel.tsx', source: aiPanel },
  { name: 'SettingsAboutPanel.tsx', source: aboutPanel },
];

describe('Settings panel ownership (no transport)', () => {
  test.each(PANEL_SOURCES)('$name must not import or call transport APIs', ({ name, source }) => {
    // SettingsSyncPanel 允许从 @/api/sync 导入类型与 pure helpers（isDeviceSucceeded 等），
    // 但禁止调用 syncApi / backupApi / invoke / 其它 transport 客户端。
    if (name === 'SettingsSyncPanel.tsx') {
      expect(source).not.toMatch(/\binvoke\s*\(/);
      expect(source).not.toContain('configApi');
      expect(source).not.toContain('healthApi');
      expect(source).not.toContain('githubTrendingApi');
      expect(source).not.toContain('orchestratorConfigApi');
      // 禁止调用面（允许注释提到名称）
      expect(source).not.toMatch(/\bsyncApi\s*\./);
      expect(source).not.toMatch(/\bbackupApi\s*\./);
      expect(source).not.toMatch(/\bpickBackupExportPath\s*\(/);
      expect(source).not.toMatch(/\bpickBackupArchivePath\s*\(/);
      // 允许的唯一 @/api 导入面：sync 类型与 helpers
      const apiImports = source.match(/from\s+'@\/api\/[^']+'/g) ?? [];
      expect(apiImports.every((line) => line.includes('@/api/sync'))).toBe(true);
      return;
    }

    expect(source).not.toContain('@/api/');
    expect(source).not.toContain('Api.');
    expect(source).not.toMatch(/\binvoke\s*\(/);
    expect(source).not.toContain("from '@/api");
    expect(source).not.toContain('configApi');
    expect(source).not.toContain('healthApi');
    expect(source).not.toContain('githubTrendingApi');
    expect(source).not.toContain('orchestratorConfigApi');
  });
});

describe('Settings controller ownership (no tab JSX trees)', () => {
  test('useSettingsController must not contain large tab panel trees', () => {
    expect(controller).not.toContain("activeTab === 'general'");
    expect(controller).not.toContain("activeTab === 'sync'");
    expect(controller).not.toContain("activeTab === 'dependencies'");
    expect(controller).not.toContain("activeTab === 'health'");
    expect(controller).not.toContain("activeTab === 'ai'");
    expect(controller).not.toContain("activeTab === 'automation'");
    expect(controller).not.toContain("activeTab === 'about'");
    expect(controller).not.toContain('settings-panel-');
    expect(controller).not.toContain('role="tabpanel"');
    expect(controller).not.toContain('<SettingsGeneralPanel');
    expect(controller).not.toContain('<SettingsSyncPanel');
    expect(controller).not.toContain('<SettingsDependenciesPanel');
    expect(controller).not.toContain('<SettingsAiPanel');
    expect(controller).not.toContain('<SettingsAboutPanel');
  });

  test('useSettingsController composes domain controllers under controllers/', () => {
    expect(controller).toContain("from './controllers/useSettingsResources'");
    expect(controller).toContain("from './controllers/useSettingsFormSaves'");
    expect(controller).toContain("from './controllers/useSettingsUpdatePermissions'");
  });
});

describe('Settings domain controllers ownership (no panel JSX)', () => {
  test.each([
    'controllers/useSettingsResources.ts',
    'controllers/useSettingsFormSaves.ts',
    'controllers/useSettingsUpdatePermissions.ts',
  ])('%s must not render tab panel JSX', (relativePath) => {
    const source = readSettingsSource(relativePath);
    expect(source).not.toContain('role="tabpanel"');
    expect(source).not.toContain('<SettingsGeneralPanel');
    expect(source).not.toContain('<SettingsSyncPanel');
    expect(source).not.toContain('<SettingsDependenciesPanel');
    expect(source).not.toContain('<SettingsAiPanel');
    expect(source).not.toContain('<SettingsAboutPanel');
  });
});

describe('Settings shell composition', () => {
  test('Settings.tsx uses controller and extracted panels', () => {
    expect(settingsShell).toContain('useSettingsController');
    expect(settingsShell).toContain('SettingsGeneralPanel');
    expect(settingsShell).toContain('SettingsSyncPanel');
    expect(settingsShell).toContain('SettingsDependenciesPanel');
    expect(settingsShell).toContain('SettingsAiPanel');
    expect(settingsShell).toContain('SettingsAboutPanel');
  });

  test('Settings.tsx does not directly import production transport modules for core orchestration', () => {
    // 编排下沉到 controller；壳层不应再直接 import config/health/github/orchestrator API
    expect(settingsShell).not.toMatch(/from\s+'@\/api\/config'/);
    expect(settingsShell).not.toMatch(/from\s+'@\/api\/health'/);
    expect(settingsShell).not.toMatch(/from\s+'@\/api\/githubTrending'/);
    expect(settingsShell).not.toMatch(/from\s+'@\/api\/orchestratorConfig'/);
  });
});
