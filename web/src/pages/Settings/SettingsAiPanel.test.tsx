// @vitest-environment jsdom
/**
 * SettingsAiPanel 交互测试
 *
 * Business Logic（为什么需要这个测试）:
 *   AI panel 是 pure props 视图；需锁住 enable toggle 与路径输入会调用 onPatch 回调，
 *   防止重构后交互静默断裂。
 *
 * Code Logic（这个测试做什么）:
 *   jsdom 渲染 SettingsAiPanel；click switch / change path 断言 mock handler 参数。
 */
import { afterEach, describe, expect, test, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { SettingsAiPanel, type SettingsAiPanelProps } from './SettingsAiPanel';
import type { GithubTrendingForm } from './settingsState';

vi.mock('react-i18next', () => ({
  useTranslation: () => ({
    t: (key: string) => key,
    i18n: { language: 'zh' },
  }),
}));

/**
 * 构造 SettingsAiPanel 最小完整 props
 *
 * Business Logic（为什么需要这个函数）:
 *   交互测试只需覆盖目标控件，其余字段用稳定默认值避免无关失败。
 *
 * Code Logic（这个函数做什么）:
 *   合并 partial override 到完整 SettingsAiPanelProps。
 *
 * @param overrides 可选 props 覆盖
 * @returns 完整 props
 */
function buildProps(overrides: Partial<SettingsAiPanelProps> = {}): SettingsAiPanelProps {
  const githubTrendingForm: GithubTrendingForm = {
    aiEnabled: true,
    claudeCliPath: 'claude',
    claudeModel: 'sonnet',
    cacheTtlHours: 24,
  };

  return {
    githubTrendingForm,
    githubTrendingConfig: null,
    claudeCliTest: null,
    githubTrendingError: null,
    testingClaudeCli: false,
    applyingGithubTrending: false,
    githubTrendingLoadError: null,
    canResetGithubTrendingDefaults: true,
    onPatchGithubTrending: vi.fn(),
    onResetGithubTrendingDefaults: vi.fn(),
    onApplyGithubTrending: vi.fn(),
    onTestClaudeCli: vi.fn(),
    onRetryGithubTrendingLoad: vi.fn(),
    retryingGithubTrending: false,
    ...overrides,
  };
}

describe('SettingsAiPanel interactions', () => {
  afterEach(() => {
    cleanup();
  });

  test('clicking AI enable switch toggles aiEnabled via onPatchGithubTrending', () => {
    const onPatchGithubTrending = vi.fn();
    const props = buildProps({
      githubTrendingForm: {
        aiEnabled: true,
        claudeCliPath: 'claude',
        claudeModel: 'sonnet',
        cacheTtlHours: 24,
      },
      onPatchGithubTrending,
    });

    render(<SettingsAiPanel {...props} />);

    const toggle = screen.getByRole('switch', {
      name: 'settings:githubTrending.aiEnabled.label',
    });
    fireEvent.click(toggle);

    expect(onPatchGithubTrending).toHaveBeenCalledTimes(1);
    expect(onPatchGithubTrending).toHaveBeenCalledWith({ aiEnabled: false });
  });

  test('typing claude path calls onPatchGithubTrending with claudeCliPath', () => {
    const onPatchGithubTrending = vi.fn();
    const props = buildProps({ onPatchGithubTrending });

    render(<SettingsAiPanel {...props} />);

    const pathInput = screen.getByLabelText('settings:githubTrending.claudeCliPath.label');
    fireEvent.change(pathInput, { target: { value: '/usr/local/bin/claude' } });

    expect(onPatchGithubTrending).toHaveBeenCalledWith({
      claudeCliPath: '/usr/local/bin/claude',
    });
  });
});
