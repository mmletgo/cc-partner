import { afterEach, describe, expect, test, vi } from 'vitest';
import type { Extension } from '@codemirror/state';
import {
  loadWorkbenchLanguage,
  __clearWorkbenchLanguageCacheForTests,
  __setWorkbenchLanguageLoaderForTests,
  __resetWorkbenchLanguageLoadersForTests,
} from './workbenchCodeEditorLanguage';

/**
 * Business Logic（为什么需要这些测试）:
 *   CodeMirror 语言扩展体积大，必须按需动态加载并复用 Promise；别名、未知语言、失败重试与
 *   快速切换时的缓存语义一旦回退，会重新把全部语言打进编辑器首包或导致高亮错乱。
 *
 * Code Logic（这个套件做什么）:
 *   用可注入 loader 验证 canonical cache 身份、未知语言 null、失败清 cache 可重试，以及真实
 *   动态 import 能解析 typescript/ts 别名；不挂载完整编辑器（组件级 stale 守卫见同目录编辑器测试）。
 */

afterEach(() => {
  __clearWorkbenchLanguageCacheForTests();
  __resetWorkbenchLanguageLoadersForTests();
});

describe('loadWorkbenchLanguage', () => {
  test('loads one language once and reuses its promise', async () => {
    const first = loadWorkbenchLanguage('typescript');
    const second = loadWorkbenchLanguage('ts');
    expect(first).toBe(second);
    await expect(first).resolves.toBeTruthy();
  });

  test('unknown language resolves to null without caching a loader', async () => {
    const first = loadWorkbenchLanguage('not-a-real-language');
    const second = loadWorkbenchLanguage('not-a-real-language');
    await expect(first).resolves.toBeNull();
    await expect(second).resolves.toBeNull();
    // 未知语言不走 Promise cache：每次返回已 resolved 的 null 新 Promise 亦可，但不得抛错
    expect(first).toBeDefined();
  });

  test('import failure rejects, clears cache, and allows retry', async () => {
    const fakeExtension = {} as Extension;
    const failing = vi.fn(() => Promise.reject(new Error('dynamic import failed')));
    const succeeding = vi.fn(() => Promise.resolve(fakeExtension));

    __setWorkbenchLanguageLoaderForTests('typescript', failing);

    const first = loadWorkbenchLanguage('typescript');
    await expect(first).rejects.toThrow('dynamic import failed');
    expect(failing).toHaveBeenCalledTimes(1);

    __setWorkbenchLanguageLoaderForTests('typescript', succeeding);

    const second = loadWorkbenchLanguage('typescript');
    expect(second).not.toBe(first);
    await expect(second).resolves.toBe(fakeExtension);
    expect(succeeding).toHaveBeenCalledTimes(1);

    // 成功后再次命中同一 Promise
    const third = loadWorkbenchLanguage('ts');
    expect(third).toBe(second);
  });

  test('canonical aliases share the same in-flight promise', () => {
    let resolveLoader!: (value: Extension) => void;
    const deferred = new Promise<Extension>((resolve) => {
      resolveLoader = resolve;
    });
    const loader = vi.fn(() => deferred);
    __setWorkbenchLanguageLoaderForTests('javascript', loader);

    const a = loadWorkbenchLanguage('js');
    const b = loadWorkbenchLanguage('javascript');
    const c = loadWorkbenchLanguage('mjs');
    expect(a).toBe(b);
    expect(b).toBe(c);
    expect(loader).toHaveBeenCalledTimes(1);

    resolveLoader({} as Extension);
  });

  test('loads yaml and shell via explicit dynamic import map', async () => {
    await expect(loadWorkbenchLanguage('yaml')).resolves.toBeTruthy();
    await expect(loadWorkbenchLanguage('yml')).resolves.toBeTruthy();
    expect(loadWorkbenchLanguage('yaml')).toBe(loadWorkbenchLanguage('yml'));

    await expect(loadWorkbenchLanguage('bash')).resolves.toBeTruthy();
    expect(loadWorkbenchLanguage('sh')).toBe(loadWorkbenchLanguage('bash'));
  });
});
