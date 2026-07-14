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
  test.each(PANEL_SOURCES)('$name must not import or call transport APIs', ({ source }) => {
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
