import { describe, expect, test } from 'vitest';
import { readFileSync } from 'node:fs';
import {
  resolveCssColorForXterm,
  withAlphaCssColor,
  workbenchTerminalOptions,
  workbenchTerminalTheme,
} from './terminalOptions';

const TOKENS_CSS_URL = new URL('../../styles/tokens.css', import.meta.url);

/**
 * Business Logic（为什么需要这个函数）:
 *   tmux attach 输出是完整 PTY 字节流，前端不能额外改写换行，否则 split 后的整屏重绘会错位。
 *
 * Code Logic（这个函数做什么）:
 *   用稳定 token stub 构造 xterm options，并断言 convertEol 保持关闭。
 */
function assertConvertEolDisabled(): void {
  const options = workbenchTerminalOptions(() => 'token');
  if (options.convertEol === true) {
    throw new Error('Workbench terminal must not enable convertEol for PTY/tmux output');
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Workbench 终端主题跟随应用浅色/深色主题，测试需要从 design token 源文件读取真实主题值。
 *
 * Code Logic（这个函数做什么）:
 *   按 CSS selector 截取声明块，再读取指定 token 的原始值；缺失时抛错暴露 token 漏配。
 */
function readThemeToken(css: string, selector: string, tokenName: string): string {
  const escapedSelector = selector.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const escapedTokenName = tokenName.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const blockMatch = css.match(new RegExp(`${escapedSelector}\\s*\\{([\\s\\S]*?)\\n\\}`));
  if (!blockMatch) {
    throw new Error(`Missing CSS selector ${selector}`);
  }
  const block = blockMatch[1] ?? '';
  const tokenMatch = block.match(new RegExp(`${escapedTokenName}\\s*:\\s*([^;]+);`));
  if (!tokenMatch) {
    throw new Error(`Missing ${tokenName} in ${selector}`);
  }
  return (tokenMatch[1] ?? '').trim();
}

/**
 * Business Logic（为什么需要这个函数）:
 *   用户切换浅色主题时，工作台终端也应切换为浅色终端，而不是继续使用深色面板。
 *
 * Code Logic（这个函数做什么）:
 *   读取 terminal 相关 design token，断言浅色与深色主题使用不同的背景、文字和边框色。
 */
function assertTerminalTokensFollowTheme(): void {
  const css = readFileSync(TOKENS_CSS_URL, 'utf8');
  const tokenNames = [
    '--terminal-bg',
    '--terminal-chrome',
    '--terminal-fg',
    '--terminal-muted',
    '--terminal-border',
  ];
  for (const tokenName of tokenNames) {
    const lightValue = readThemeToken(css, ':root', tokenName);
    const darkValue = readThemeToken(css, '[data-theme="dark"]', tokenName);
    if (lightValue === darkValue) {
      throw new Error(`${tokenName} must differ between light and dark themes`);
    }
  }
}

describe('terminalOptions', () => {
  test('keeps convertEol disabled and binds terminal tokens to light/dark theme', () => {
    assertConvertEolDisabled();
    assertTerminalTokensFollowTheme();
  });

  test('selection colors are xterm-parseable rgba, not raw color-mix tokens', () => {
    // Business Logic: color-mix 会被 xterm 静默丢弃，浅色主题下选区几乎不可见。
    const theme = workbenchTerminalTheme((name, fallback) => {
      if (name === '--accent') return '#c96442';
      if (name === '--terminal-bg') return '#faf9f5';
      if (name === '--terminal-fg') return '#141413';
      if (name === '--accent-soft') return 'color-mix(in oklab, #c96442 14%, transparent)';
      return fallback;
    });
    expect(theme.selectionBackground).toMatch(/^rgba\(/);
    expect(theme.selectionInactiveBackground).toMatch(/^rgba\(/);
    expect(theme.selectionBackground).not.toContain('color-mix');
  });

  test('resolveCssColorForXterm keeps hex/rgb and falls back for unresolvable values', () => {
    expect(resolveCssColorForXterm('#abc', '#000')).toBe('#abc');
    expect(resolveCssColorForXterm('rgb(1, 2, 3)', '#000')).toBe('rgb(1, 2, 3)');
    expect(withAlphaCssColor('#c96442', 0.4, 'rgba(0,0,0,0.3)')).toBe('rgba(201, 100, 66, 0.4)');
    expect(withAlphaCssColor('rgb(10, 20, 30)', 0.5, 'rgba(0,0,0,0.3)')).toBe(
      'rgba(10, 20, 30, 0.5)',
    );
  });

  test('enables macOptionClickForcesSelection so mouse-mode TUIs still allow text selection', () => {
    const options = workbenchTerminalOptions(() => 'token');
    expect(options.macOptionClickForcesSelection).toBe(true);
    expect(options.rightClickSelectsWord).toBe(true);
  });
});
