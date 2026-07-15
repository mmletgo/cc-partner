/**
 * WorkbenchSessionSearch 有界搜索结果契约测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   search 现返回 SessionSearchResult；UI 必须渲染 items，并在 truncated/unavailable
 *   时展示非阻塞诊断横幅，同时 hooks 顺序不得因 open 切换崩溃。
 *
 * Code Logic（这个测试做什么）:
 *   mock workbenchApi.claudeSessions.search；用 I18nextProvider + waitFor 覆盖
 *   debounce 后的结果渲染与诊断文案。
 */

// @vitest-environment jsdom

import { afterEach, beforeAll, beforeEach, describe, expect, test, vi } from 'vitest';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { I18nextProvider } from 'react-i18next';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import i18n from '@/i18n';
import type {
  SessionSearchHit,
  SessionSearchResult,
} from '@/lib/types';
import {
  WorkbenchSessionSearch,
  type WorkbenchSessionSearchProps,
} from './WorkbenchSessionSearch';

const searchMock = vi.fn();
const previewMock = vi.fn();
const resumeMock = vi.fn();

vi.mock('@/api/workbench', () => ({
  workbenchApi: {
    claudeSessions: {
      search: (...args: unknown[]) => searchMock(...args),
      preview: (...args: unknown[]) => previewMock(...args),
      resume: (...args: unknown[]) => resumeMock(...args),
    },
  },
}));

beforeAll(async () => {
  await i18n.changeLanguage('zh');
});

beforeEach(() => {
  searchMock.mockReset();
  previewMock.mockReset();
  resumeMock.mockReset();
});

afterEach(() => {
  cleanup();
});

/**
 * Business Logic（为什么需要这个函数）:
 *   多个用例共享最小合法命中条目。
 *
 * Code Logic（这个函数做什么）:
 *   返回可覆盖字段的 SessionSearchHit。
 */
function buildHit(overrides: Partial<SessionSearchHit> = {}): SessionSearchHit {
  const now = new Date().toISOString();
  return {
    sessionId: 'abc-123-session',
    title: 'Fix login bug',
    titleHit: true,
    userHit: false,
    assistantHit: false,
    firstActivityAt: now,
    lastActivityAt: now,
    messageCount: 3,
    previewSnippets: ['login failed'],
    ...overrides,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   契约测试需要稳定的有界搜索 DTO。
 *
 * Code Logic（这个函数做什么）:
 *   组装 SessionSearchResult，允许覆盖 truncated/diagnostics/items。
 */
function buildResult(overrides: Partial<SessionSearchResult> = {}): SessionSearchResult {
  return {
    items: [buildHit()],
    truncated: false,
    diagnostics: {
      status: 'ok',
      reasons: [],
      filesConsidered: 1,
      filesIndexed: 1,
      bytesRead: 128,
    },
    ...overrides,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   挂载 i18n 与默认 props，避免文案 key 泄漏。
 *
 * Code Logic（这个函数做什么）:
 *   用 I18nextProvider 渲染 WorkbenchSessionSearch。
 */
function renderSearch(props: Partial<WorkbenchSessionSearchProps> = {}) {
  const defaults: WorkbenchSessionSearchProps = {
    open: true,
    onClose: vi.fn(),
    projectId: 'p1',
    worktreeId: 'wt1',
    offline: false,
    worktreeName: 'main',
    onResumed: vi.fn(),
  };
  return render(
    <I18nextProvider i18n={i18n}>
      <WorkbenchSessionSearch {...defaults} {...props} />
    </I18nextProvider>,
  );
}

describe('WorkbenchSessionSearch', () => {
  test('renders hits from result.items', async () => {
    searchMock.mockResolvedValue(buildResult());
    renderSearch();
    await waitFor(
      () => {
        expect(screen.getByText('Fix login bug')).toBeTruthy();
      },
      { timeout: 2000 },
    );
    expect(searchMock).toHaveBeenCalledWith('p1', 'wt1', '');
  });

  test('shows truncated diagnostics when truncated=true', async () => {
    searchMock.mockResolvedValue(
      buildResult({
        truncated: true,
        diagnostics: {
          status: 'truncated',
          reasons: ['max_files', 'max_total_bytes', 'unknown_reason'],
          filesConsidered: 40,
          filesIndexed: 20,
          bytesRead: 4096,
        },
      }),
    );
    renderSearch();
    await waitFor(
      () => {
        expect(screen.getByText('索引已截断（达到预算），结果可能不完整')).toBeTruthy();
      },
      { timeout: 2000 },
    );
    expect(screen.getByText('Fix login bug')).toBeTruthy();
    expect(screen.getByText('文件数上限')).toBeTruthy();
    expect(screen.getByText('总读取字节上限')).toBeTruthy();
    expect(screen.queryByText('unknown_reason')).toBeNull();
  });

  test('shows diagnostics unavailable messaging when status=unavailable', async () => {
    searchMock.mockResolvedValue(
      buildResult({
        truncated: false,
        diagnostics: {
          status: 'unavailable',
          reasons: [],
          filesConsidered: 0,
          filesIndexed: 0,
          bytesRead: 0,
        },
      }),
    );
    renderSearch();
    await waitFor(
      () => {
        expect(screen.getByText('对端未提供截断诊断')).toBeTruthy();
      },
      { timeout: 2000 },
    );
    expect(screen.getByText('Fix login bug')).toBeTruthy();
    expect(screen.queryByText('索引已截断（达到预算），结果可能不完整')).toBeNull();
  });

  test('hooks stay before early returns (source scan + open toggle)', async () => {
    searchMock.mockResolvedValue(buildResult());
    // open=false 与 open=true 均不应因 hooks 顺序崩溃
    const { rerender } = renderSearch({ open: false });
    rerender(
      <I18nextProvider i18n={i18n}>
        <WorkbenchSessionSearch
          open={true}
          onClose={vi.fn()}
          projectId="p1"
          worktreeId="wt1"
          offline={false}
          worktreeName="main"
          onResumed={vi.fn()}
        />
      </I18nextProvider>,
    );
    await waitFor(
      () => {
        expect(screen.getByText('Fix login bug')).toBeTruthy();
      },
      { timeout: 2000 },
    );

    const sourcePath = path.join(
      path.dirname(fileURLToPath(import.meta.url)),
      'WorkbenchSessionSearch.tsx',
    );
    const source = readFileSync(sourcePath, 'utf8');
    // 粗扫描：组件导出函数体内不应出现 early return 后再声明 useState/useEffect
    const exportIdx = source.indexOf('export function WorkbenchSessionSearch');
    expect(exportIdx).toBeGreaterThanOrEqual(0);
    const body = source.slice(exportIdx);
    const earlyReturnMatch = body.match(/\nif\s*\([^)]*\)\s*return\b/);
    if (earlyReturnMatch && earlyReturnMatch.index != null) {
      const afterReturn = body.slice(earlyReturnMatch.index);
      expect(afterReturn).not.toMatch(/\n\s*const\s+\[[^\]]+\]\s*=\s*useState/);
      expect(afterReturn).not.toMatch(/\n\s*useEffect\s*\(/);
    }
  });
});
