import {
  isPreviewAssetUrlEligible,
  rewriteCssAssetUrls,
  rewriteHtmlPreviewAssets,
} from './htmlAssets';
// @ts-expect-error - 本仓库 tsconfig 未在 compilerOptions.types 纳入 node,node:process 类型缺失,运行时 tsx 正常
import { exit } from 'node:process';
import type { WorkbenchHtmlAsset } from '@/lib/types';

/**
 * Business Logic（为什么需要这个函数）:
 *   HTML 资源重写 helper 的脚本测试需要清晰失败原因，便于在无测试框架环境下快速定位回归。
 *
 * Code Logic（这个函数做什么）:
 *   条件为 false 时抛出 Error，使 tsx 进程以非零状态退出。
 */
function assert(condition: boolean, message: string): void {
  if (!condition) throw new Error(message);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   测试需要模拟后端只读资源命令，覆盖 HTML 文档路径与 CSS 文档路径两类相对解析。
 *
 * Code Logic（这个函数做什么）:
 *   使用 `documentPath + assetPath` 作为 key 查询内存资源表，找不到时返回 null。
 */
function createAssetLoader(
  assets: Record<string, WorkbenchHtmlAsset>,
): (documentPath: string, assetPath: string) => Promise<WorkbenchHtmlAsset | null> {
  return async (documentPath: string, assetPath: string) => assets[`${documentPath}\0${assetPath}`] ?? null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   HTML 预览会把重写后的 CSS 作为 base64 data URL 写入 href，测试需要读取其中的 CSS 内容验证二级资源。
 *
 * Code Logic（这个函数做什么）:
 *   从 HTML 字符串提取 text/css data URL，base64 解码后返回 CSS 文本。
 */
function decodeFirstCssDataUrl(html: string): string {
  const match = /href="data:text\/css;base64,([^"]+)"/.exec(html);
  if (!match) throw new Error('missing css data url');
  return atob(match[1]);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   外链、data/blob URL、锚点和绝对路径不能被 Workbench HTML 预览资源命令读取或继续加载。
 *
 * Code Logic（这个函数做什么）:
 *   调用资源 URL eligibility helper，断言仅项目内相对路径会进入后端加载流程。
 */
function testAssetUrlEligibility(): void {
  assert(isPreviewAssetUrlEligible('./style.css'), 'relative stylesheet is eligible');
  assert(isPreviewAssetUrlEligible('../assets/logo.png'), 'parent relative image is eligible');
  assert(isPreviewAssetUrlEligible('images/logo 2x.png'), 'relative path with space is eligible');
  assert(!isPreviewAssetUrlEligible('https://example.com/logo.png'), 'https URL is rejected');
  assert(!isPreviewAssetUrlEligible('data:image/png;base64,abc'), 'data URL is rejected');
  assert(!isPreviewAssetUrlEligible('blob:https://example.com/id'), 'blob URL is rejected');
  assert(!isPreviewAssetUrlEligible('#icon'), 'fragment-only URL is rejected');
  assert(!isPreviewAssetUrlEligible('/etc/passwd'), 'root absolute path is rejected');
  assert(!isPreviewAssetUrlEligible('\\\\server\\share\\secret.css'), 'Windows UNC path is rejected');
  assert(!isPreviewAssetUrlEligible('\\windows-root\\secret.css'), 'Windows root path is rejected');
  assert(!isPreviewAssetUrlEligible('C:\\Users\\x\\secret.txt'), 'Windows absolute path is rejected');
}

/**
 * Business Logic（为什么需要这个函数）:
 *   外部 CSS 文件中的相对图片资源应按 CSS 文件自身位置解析，而不是按 HTML 文件位置解析。
 *
 * Code Logic（这个函数做什么）:
 *   模拟 CSS 文档 `docs/styles/site.css`，断言相对 `url()` 被替换为 data URL，外链被清空。
 */
async function testCssUrlRewrite(): Promise<void> {
  const loadAsset = createAssetLoader({
    'docs/styles/site.css\0../assets/bg.png': {
      path: 'docs/assets/bg.png',
      mime: 'image/png',
      size: 3,
      dataUrl: 'data:image/png;base64,YmJn',
      text: null,
    },
  });
  const css = 'body{background:url("../assets/bg.png")} .remote{background:url("https://example.com/a.png")}';

  const rewritten = await rewriteCssAssetUrls(css, 'docs/styles/site.css', loadAsset);

  assert(
    rewritten.includes('url("data:image/png;base64,YmJn")'),
    'relative CSS url is replaced with data URL',
  );
  assert(!rewritten.includes('https://example.com/a.png'), 'external CSS url is removed');
  assert(rewritten.includes('.remote{background:url("")}'), 'external CSS url keeps CSS valid');
}

/**
 * Business Logic（为什么需要这个函数）:
 *   HTML 预览需要把核心相对资源替换为 data URL，iframe 才能在 sandbox srcDoc 中展示项目内样式和图片。
 *
 * Code Logic（这个函数做什么）:
 *   模拟 HTML、CSS、图片三类资源，断言 link/img/poster/srcset 被改写，外部资源不会继续留在 srcDoc 中。
 */
async function testHtmlAssetRewrite(): Promise<void> {
  const loadAsset = createAssetLoader({
    'docs/page.html\0./styles/site.css': {
      path: 'docs/styles/site.css',
      mime: 'text/css',
      size: 45,
      dataUrl: 'data:text/css;base64,ignored',
      text: '.hero{background:url("../assets/bg.png")}',
    },
    'docs/styles/site.css\0../assets/bg.png': {
      path: 'docs/assets/bg.png',
      mime: 'image/png',
      size: 3,
      dataUrl: 'data:image/png;base64,YmJn',
      text: null,
    },
    'docs/page.html\0../assets/logo.png': {
      path: 'assets/logo.png',
      mime: 'image/png',
      size: 4,
      dataUrl: 'data:image/png;base64,bG9nbw==',
      text: null,
    },
  });
  const html = [
    '<link rel="stylesheet" href="./styles/site.css">',
    '<img src="../assets/logo.png" srcset="../assets/logo.png 1x, https://example.com/logo@2x.png 2x">',
    '<video poster="https://example.com/poster.png"><source src="../assets/logo.png"></video>',
  ].join('');

  const rewritten = await rewriteHtmlPreviewAssets(html, {
    documentPath: 'docs/page.html',
    loadAsset,
  });

  assert(rewritten.includes('href="data:text/css;base64,'), 'stylesheet href is rewritten to CSS data URL');
  assert(
    decodeFirstCssDataUrl(rewritten).includes('data:image/png;base64,YmJn'),
    'stylesheet inner relative url is rewritten',
  );
  assert(rewritten.includes('src="data:image/png;base64,bG9nbw=="'), 'image src is rewritten');
  assert(rewritten.includes('srcset="data:image/png;base64,bG9nbw== 1x"'), 'srcset keeps only safe candidates');
  assert(!rewritten.includes('https://example.com'), 'external preview resources are removed');
}

/**
 * Business Logic（为什么需要这个函数）:
 *   HTML 的 `<link>` 既可能是 stylesheet，也可能是 canonical 这类文档关系；预览资源重写不能误伤非渲染链接。
 *
 * Code Logic（这个函数做什么）:
 *   构造 canonical link 并提供会抛错的 loader，断言 helper 不会请求该 href 且保持原属性。
 */
async function testNonResourceLinkIsPreserved(): Promise<void> {
  const html = '<link rel="canonical" href="./page.html"><link rel="alternate" href="./feed.xml">';
  const rewritten = await rewriteHtmlPreviewAssets(html, {
    documentPath: 'docs/page.html',
    loadAsset: async () => {
      throw new Error('non-resource link should not load asset');
    },
  });

  assert(
    rewritten.includes('rel="canonical" href="./page.html"'),
    'canonical link href is preserved',
  );
  assert(rewritten.includes('rel="alternate" href="./feed.xml"'), 'alternate link href is preserved');
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Workbench HTML 资源重写是 iframe 预览的安全边界，测试入口需要覆盖 URL、CSS 和 HTML 三类行为。
 *
 * Code Logic（这个函数做什么）:
 *   顺序执行所有 helper 测试，任一断言失败都会让进程失败。
 */
async function main(): Promise<void> {
  testAssetUrlEligibility();
  await testCssUrlRewrite();
  await testHtmlAssetRewrite();
  await testNonResourceLinkIsPreserved();
}

void main()
  .then(() => {
    exit(0);
  })
  .catch((error: unknown) => {
    console.error(error);
    exit(1);
  });
