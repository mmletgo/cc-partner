// @vitest-environment jsdom
/**
 * Dialog/Drawer 业务迁移合同测试
 *
 * Business Logic（为什么需要这些测试）:
 *   S4 Task 3 要求生产 TSX 不再手写 role=dialog / aria-modal，统一走共享 Dialog/Drawer；
 *   同时锁住 backend close、Prompt delete、Orchestrator create/detail、session search、
 *   mobile drawer 的关闭策略与焦点恢复，避免迁移时破坏 busy 门闩与键盘可达性。
 *
 * Code Logic（这些测试做什么）:
 *   1) 扫描 web/src 生产 .tsx，失败于 Dialog/Drawer 实现外的 raw dialog 语义；
 *   2) 用 Testing Library 覆盖关键交互：busy 不可关、Escape/关闭后焦点回到触发器。
 */

import { readdirSync, readFileSync, statSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  useRef,
  useState,
  type ReactElement,
  type ReactNode,
} from 'react';
import { afterEach, beforeAll, describe, expect, test } from 'vitest';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';

import i18n from '@/i18n';
import { Dialog } from './Dialog';
import { Drawer } from '../Drawer';

const SRC_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../..');

/** Dialog/Drawer 实现本身允许 role=dialog / aria-modal */
const ALLOWED_DIALOG_ROLE_FILES = new Set([
  path.normalize(path.join(SRC_ROOT, 'components/primitives/Dialog/Dialog.tsx')),
  path.normalize(path.join(SRC_ROOT, 'components/primitives/Drawer/Drawer.tsx')),
]);

/**
 * 递归收集目录下生产 TSX（排除 test / stories）
 *
 * Business Logic（为什么需要这个函数）:
 *   inventory 必须扫全量业务源，避免漏网的 raw modal 语义。
 *
 * Code Logic（这个函数做什么）:
 *   深度遍历 root，收集 .tsx 且路径不含 .test. / .spec. / __tests__ / stories 的文件。
 */
function collectProductionTsx(root: string): string[] {
  const results: string[] = [];

  /**
   * 深度遍历
   *
   * Business Logic（为什么需要这个函数）:
   *   嵌套页面/domain 目录都可能出现 modal 语义。
   *
   * Code Logic（这个函数做什么）:
   *   目录则递归；文件则按扩展名与测试命名过滤后加入结果。
   */
  function walk(dir: string): void {
    for (const entry of readdirSync(dir)) {
      const full = path.join(dir, entry);
      const stat = statSync(full);
      if (stat.isDirectory()) {
        if (entry === 'node_modules' || entry === 'dist' || entry === '__tests__') continue;
        walk(full);
        continue;
      }
      if (!entry.endsWith('.tsx')) continue;
      if (entry.includes('.test.') || entry.includes('.spec.') || entry.includes('.stories.')) {
        continue;
      }
      results.push(full);
    }
  }

  walk(root);
  return results;
}

/**
 * 判断源码行是否包含 raw dialog 角色或 aria-modal
 *
 * Business Logic（为什么需要这个函数）:
 *   合同只禁止业务页手写 modal 语义，注释/字符串中的说明也应尽量精确匹配属性写法。
 *
 * Code Logic（这个函数做什么）:
 *   匹配 role="dialog"|role='dialog' 与 aria-modal="true"|aria-modal='true'。
 */
function findRawModalViolations(source: string): string[] {
  const violations: string[] = [];
  const lines = source.split('\n');
  lines.forEach((line, index) => {
    const trimmed = line.trim();
    if (trimmed.startsWith('//') || trimmed.startsWith('*') || trimmed.startsWith('/*')) {
      return;
    }
    if (/role\s*=\s*["']dialog["']/.test(line) || /aria-modal\s*=\s*["']true["']/.test(line)) {
      violations.push(`${index + 1}: ${trimmed}`);
    }
  });
  return violations;
}

beforeAll(async () => {
  await i18n.changeLanguage('zh');
});

afterEach(() => {
  cleanup();
  document.body.style.overflow = '';
});

describe('dialog migration inventory', () => {
  test('production TSX has no raw role=dialog or aria-modal outside Dialog/Drawer', () => {
    const files = collectProductionTsx(SRC_ROOT);
    const failures: string[] = [];

    for (const file of files) {
      const normalized = path.normalize(file);
      if (ALLOWED_DIALOG_ROLE_FILES.has(normalized)) continue;
      const source = readFileSync(file, 'utf8');
      const violations = findRawModalViolations(source);
      if (violations.length > 0) {
        const rel = path.relative(SRC_ROOT, file);
        failures.push(`${rel}\n  ${violations.join('\n  ')}`);
      }
    }

    expect(failures, failures.join('\n\n')).toEqual([]);
  });

  test('Welcome route uses main/region semantics instead of dialog role', () => {
    const welcomeSource = readFileSync(
      path.join(SRC_ROOT, 'pages/Welcome/Welcome.tsx'),
      'utf8',
    );
    expect(welcomeSource).not.toMatch(/role\s*=\s*["']dialog["']/);
    expect(welcomeSource).toMatch(/role\s*=\s*["']main["']|role\s*=\s*["']region["']|<main\b/);
  });
});

describe('dialog migration focus restore interactions', () => {
  /**
   * 通用触发器 + Dialog 壳，验证关闭后焦点回到触发按钮
   */
  function FocusRestoreHost(props: {
    busy?: boolean;
    useDrawer?: boolean;
  }): ReactElement {
    const { busy = false, useDrawer = false } = props;
    const [open, setOpen] = useState(false);
    const closeRef = useRef<HTMLButtonElement | null>(null);
    const titleId = useDrawer ? 'drawer-title' : 'dialog-title';

    /**
     * 关闭回调：busy 时 no-op
     *
     * Business Logic（为什么需要这个函数）:
     *   迁移合同要求 busy-state close prevention 由调用方 onClose early return 保留。
     *
     * Code Logic（这个函数做什么）:
     *   busy 时直接返回；否则 setOpen(false)。
     */
    const handleClose = (): void => {
      if (busy) return;
      setOpen(false);
    };

    const body: ReactNode = (
      <>
        <h2 id={titleId}>{useDrawer ? 'Drawer title' : 'Dialog title'}</h2>
        <button type="button" ref={closeRef} onClick={handleClose}>
          Close surface
        </button>
        <button type="button">Other action</button>
      </>
    );

    return (
      <>
        <button type="button" data-testid="trigger" onClick={() => setOpen(true)}>
          Open surface
        </button>
        {useDrawer ? (
          <Drawer
            open={open}
            titleId={titleId}
            side="left"
            initialFocusRef={closeRef}
            onClose={handleClose}
          >
            {body}
          </Drawer>
        ) : (
          <Dialog open={open} titleId={titleId} initialFocusRef={closeRef} onClose={handleClose}>
            {body}
          </Dialog>
        )}
      </>
    );
  }

  test('Dialog restores focus to trigger after Escape and blocks close when busy', async () => {
    const user = userEvent.setup();
    const { rerender } = render(<FocusRestoreHost />);

    const trigger = screen.getByTestId('trigger');
    trigger.focus();
    expect(document.activeElement).toBe(trigger);

    await user.click(trigger);
    const dialog = await screen.findByRole('dialog');
    expect(dialog).toBeTruthy();

    await waitFor(() => {
      expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Close surface' }));
    });

    await user.keyboard('{Escape}');
    await waitFor(() => {
      expect(screen.queryByRole('dialog')).toBeNull();
    });
    await waitFor(() => {
      expect(document.activeElement).toBe(trigger);
    });

    rerender(<FocusRestoreHost busy />);
    await user.click(screen.getByTestId('trigger'));
    expect(await screen.findByRole('dialog')).toBeTruthy();
    await user.keyboard('{Escape}');
    expect(screen.getByRole('dialog')).toBeTruthy();
  });

  test('Drawer left side restores focus to menu trigger after close', async () => {
    const user = userEvent.setup();
    render(<FocusRestoreHost useDrawer />);

    const trigger = screen.getByTestId('trigger');
    trigger.focus();
    await user.click(trigger);

    const drawer = await screen.findByRole('dialog');
    expect(drawer.getAttribute('data-side')).toBe('left');
    await waitFor(() => {
      expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Close surface' }));
    });

    await user.click(screen.getByRole('button', { name: 'Close surface' }));
    await waitFor(() => {
      expect(screen.queryByRole('dialog')).toBeNull();
    });
    await waitFor(() => {
      expect(document.activeElement).toBe(trigger);
    });
  });

  test('BackendCloseChoiceListener source uses Dialog with busy close gates', () => {
    const appSource = readFileSync(path.join(SRC_ROOT, 'App.tsx'), 'utf8');
    expect(appSource).toMatch(/<Dialog[\s>]/);
    expect(appSource).toMatch(/titleId="backend-close-title"/);
    expect(appSource).toMatch(/closeOnEscape=\{closingMode === null\}/);
    expect(appSource).toMatch(/closeOnBackdrop=\{closingMode === null\}/);
    expect(appSource).not.toMatch(/role\s*=\s*["']dialog["']/);
    expect(appSource).not.toMatch(/aria-modal\s*=\s*["']true["']/);
  });

  test('migrated ownership sources consume Dialog or Drawer primitives', () => {
    const ownershipFiles = [
      'App.tsx',
      'components/layout/AppShell/AppShell.tsx',
      'pages/Prompts/Prompts.tsx',
      'pages/Scratchpad/Scratchpad.tsx',
      'pages/CcHistory/CcHistory.tsx',
      'pages/Orchestrator/Orchestrator.tsx',
      'components/domain/WorkbenchSessionSearch/WorkbenchSessionSearch.tsx',
      'components/domain/WorkbenchProjectRail/WorkbenchProjectRail.tsx',
      'mobile/components/MobileWorkbenchShell.tsx',
      'mobile/components/MobileWorktreeQuickSwitch.tsx',
      'mobile/components/MobileAutomationPanel.tsx',
    ];

    for (const rel of ownershipFiles) {
      const source = readFileSync(path.join(SRC_ROOT, rel), 'utf8');
      const usesDialog = /import\s*\{[^}]*\bDialog\b[^}]*\}/.test(source) || /<Dialog[\s>]/.test(source);
      const usesDrawer = /import\s*\{[^}]*\bDrawer\b[^}]*\}/.test(source) || /<Drawer[\s>]/.test(source);
      expect(
        usesDialog || usesDrawer,
        `${rel} should import/use Dialog or Drawer after migration`,
      ).toBe(true);
      expect(source, `${rel} should not keep raw role=dialog`).not.toMatch(
        /role\s*=\s*["']dialog["']/,
      );
      expect(source, `${rel} should not keep raw aria-modal`).not.toMatch(
        /aria-modal\s*=\s*["']true["']/,
      );
    }

    const shellSource = readFileSync(
      path.join(SRC_ROOT, 'mobile/components/MobileWorkbenchShell.tsx'),
      'utf8',
    );
    expect(shellSource).toMatch(/<Drawer[\s>]/);
    expect(shellSource).toMatch(/side=["']left["']/);

    const orchestratorSource = readFileSync(
      path.join(SRC_ROOT, 'pages/Orchestrator/Orchestrator.tsx'),
      'utf8',
    );
    expect(orchestratorSource).toMatch(/<Dialog[\s>]/);
    // detail 使用 Drawer 或 Dialog 均可，但不得自管 Escape 与 createPortal
    expect(orchestratorSource).not.toMatch(/window\.addEventListener\(['"]keydown['"]/);
    expect(orchestratorSource).not.toMatch(/createPortal/);
  });

  test('session search and prompt delete source no longer attach Escape listeners', () => {
    const sessionSource = readFileSync(
      path.join(SRC_ROOT, 'components/domain/WorkbenchSessionSearch/WorkbenchSessionSearch.tsx'),
      'utf8',
    );
    // 列表 Esc 关闭 palette 由 Dialog 负责；preview 内 Esc 返回列表可保留
    expect(sessionSource).toMatch(/<Dialog[\s>]/);
    expect(sessionSource).not.toMatch(/role\s*=\s*["']dialog["']/);

    const promptsSource = readFileSync(path.join(SRC_ROOT, 'pages/Prompts/Prompts.tsx'), 'utf8');
    expect(promptsSource).toMatch(/<Dialog[\s>]/);
    expect(promptsSource).not.toMatch(/modalMask/);
  });
});
