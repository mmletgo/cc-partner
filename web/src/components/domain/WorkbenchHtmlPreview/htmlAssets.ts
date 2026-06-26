import type { WorkbenchHtmlAsset } from '@/lib/types';

/**
 * HTML 预览资源读取器。
 *
 * Business Logic（为什么需要这个类型）:
 *   资源重写 helper 位于 domain 组件目录，不能直接知道当前 Workbench 的 project/worktree 状态。
 *
 * Code Logic（这个类型做什么）:
 *   约定调用方按文档路径和资源引用异步返回后端生成的 data URL，失败或不允许时返回 null。
 */
export type WorkbenchHtmlAssetLoader = (
  documentPath: string,
  assetPath: string,
) => Promise<WorkbenchHtmlAsset | null>;

/**
 * HTML 预览资源重写配置。
 *
 * Business Logic（为什么需要这个接口）:
 *   每次 HTML 预览都必须绑定当前打开文件路径和当前 worktree 的资源加载能力。
 *
 * Code Logic（这个接口做什么）:
 *   聚合当前 HTML 文档路径与资源读取函数，供 HTML/CSS 多层重写共享。
 */
export interface RewriteHtmlPreviewAssetsOptions {
  documentPath: string;
  loadAsset: WorkbenchHtmlAssetLoader;
}

/**
 * HTML 预览资源重写上下文。
 *
 * Business Logic（为什么需要这个接口）:
 *   HTML 标签和 CSS url() 的递归重写需要共享同一套路径解析与资源读取上下文。
 *
 * Code Logic（这个接口做什么）:
 *   在内部函数之间传递当前文档路径和资源 loader。
 */
interface HtmlRewriteContext {
  documentPath: string;
  loadAsset: WorkbenchHtmlAssetLoader;
}

/** 异步正则替换回调。 */
type AsyncMatchReplacer = (match: RegExpExecArray) => Promise<string>;

/** HTML 起始标签匹配；用于在不依赖浏览器 DOM 的 Node 测试中做轻量属性重写。 */
const HTML_TAG_PATTERN = /<([a-zA-Z][\w:-]*)(\s[^<>]*?)?>/g;
/** 内联 style block 匹配；需要先处理，避免普通标签扫描误改 style 标签属性。 */
const STYLE_BLOCK_PATTERN = /<style\b([^>]*)>([\s\S]*?)<\/style>/gi;
/** CSS url(...) 匹配，支持单双引号和无引号 URL。 */
const CSS_URL_PATTERN = /url\(\s*(?:"([^"]*)"|'([^']*)'|([^'")]*?))\s*\)/gi;
/** Markdown inline image 匹配；覆盖 `![alt](url "title")` 的常见写法。 */
const MARKDOWN_INLINE_IMAGE_PATTERN =
  /!\[([^\]\n]*)\]\(\s*(<([^>\n]+)>|([^)\s]+))(?:\s+((?:"[^"\n]*"|'[^'\n]*'|\([^)\n]*\))))?\s*\)/g;
/** Markdown reference image 使用匹配；用于收集哪些 label 是图片引用而不是普通链接。 */
const MARKDOWN_REFERENCE_IMAGE_USE_PATTERN = /!\[([^\]\n]*)\]\[([^\]\n]*)\]/g;
/** Markdown reference definition 匹配；只重写被图片引用使用过的定义。 */
const MARKDOWN_REFERENCE_DEFINITION_PATTERN =
  /^([ \t]{0,3})\[([^\]\n]+)\]:[ \t]*(<([^>\n]+)>|(\S+))(?:[ \t]+((?:"[^"\n]*"|'[^'\n]*'|\([^)\n]*\))))?[ \t]*$/gm;
/** srcset 候选项内部 URL 与 descriptor 的分隔符。 */
const SRCSET_SEPARATOR_PATTERN = /\s+/;

/** 每类 HTML 标签中可能触发资源加载的属性集合。 */
const RESOURCE_ATTRIBUTES_BY_TAG: Record<string, readonly string[]> = {
  audio: ['src'],
  embed: ['src'],
  iframe: ['src'],
  img: ['src', 'srcset'],
  input: ['src'],
  link: ['href'],
  object: ['data'],
  script: ['src'],
  source: ['src', 'srcset'],
  track: ['src'],
  video: ['src', 'poster'],
};

/** 需要改写 href 的 link rel 值；canonical 等非渲染资源链接会保持原样。 */
const REWRITABLE_LINK_RELS = new Set([
  'apple-touch-icon',
  'dns-prefetch',
  'icon',
  'manifest',
  'modulepreload',
  'preconnect',
  'prefetch',
  'preload',
  'shortcut',
  'stylesheet',
]);

/**
 * Business Logic（为什么需要这个函数）:
 *   HTML 预览只能请求项目内相对资源，外链、data/blob 和绝对路径不应进入后端资源读取命令。
 *
 * Code Logic（这个函数做什么）:
 *   清洗 URL 字符串，拒绝 URL scheme、协议相对 URL、fragment-only、Unix 绝对路径和 Windows 盘符路径。
 */
export function isPreviewAssetUrlEligible(url: string): boolean {
  const trimmed = url.trim();
  if (
    !trimmed ||
    trimmed.startsWith('#') ||
    trimmed.startsWith('//') ||
    trimmed.startsWith('\\')
  ) {
    return false;
  }
  if (/^[a-zA-Z][a-zA-Z0-9+.-]*:/.test(trimmed)) return false;
  if (trimmed.startsWith('/')) return false;
  if (/^[a-zA-Z]:[\\/]/.test(trimmed)) return false;
  return true;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   CSS 文本里的 url() 资源也需要内联，否则外部 CSS 变成 data URL 后其相对图片和字体会失去文件基准路径。
 *
 * Code Logic（这个函数做什么）:
 *   异步扫描 CSS url()，按 CSS 文件自身 documentPath 加载相对资源；不可访问或不允许的 URL 改写为空 url。
 */
export async function rewriteCssAssetUrls(
  css: string,
  documentPath: string,
  loadAsset: WorkbenchHtmlAssetLoader,
): Promise<string> {
  return replaceAsync(css, CSS_URL_PATTERN, async (match) => {
    const rawUrl = (match[1] ?? match[2] ?? match[3] ?? '').trim();
    if (!isPreviewAssetUrlEligible(rawUrl)) {
      return 'url("")';
    }

    const asset = await loadAsset(documentPath, rawUrl);
    if (!asset) {
      return 'url("")';
    }

    return `url("${asset.dataUrl}")`;
  });
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Workbench HTML iframe 使用 srcDoc 渲染，必须先把项目内相对资源改写为 data URL 才能在 sandbox 中预览。
 *
 * Code Logic（这个函数做什么）:
 *   先重写 `<style>` 内联 CSS，再扫描核心资源标签的 src/href/poster/srcset/data 属性并替换为内联资源。
 */
export async function rewriteHtmlPreviewAssets(
  html: string,
  options: RewriteHtmlPreviewAssetsOptions,
): Promise<string> {
  const context: HtmlRewriteContext = {
    documentPath: options.documentPath,
    loadAsset: options.loadAsset,
  };
  const htmlWithStyles = await rewriteInlineStyleBlocks(html, context);

  return replaceAsync(htmlWithStyles, HTML_TAG_PATTERN, async (match) => {
    const tagName = match[1]?.toLowerCase() ?? '';
    const attributes = RESOURCE_ATTRIBUTES_BY_TAG[tagName];
    if (!attributes) return match[0];

    let rewrittenTag = match[0];
    for (const attributeName of attributes) {
      if (!shouldRewriteResourceAttribute(tagName, attributeName, rewrittenTag)) continue;
      rewrittenTag = await rewriteHtmlAttribute(rewrittenTag, tagName, attributeName, context);
    }
    return rewrittenTag;
  });
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Markdown 预览中的图片同样需要加载项目内相对资源，但不能让预览层把外链或根外文件带入渲染。
 *
 * Code Logic（这个函数做什么）:
 *   重写 inline image 和被图片引用使用过的 reference definition；普通链接保持原样，不安全或读取失败的图片 URL 置空。
 */
export async function rewriteMarkdownPreviewAssets(
  markdown: string,
  options: RewriteHtmlPreviewAssetsOptions,
): Promise<string> {
  const context: HtmlRewriteContext = {
    documentPath: options.documentPath,
    loadAsset: options.loadAsset,
  };
  const imageReferenceLabels = collectMarkdownImageReferenceLabels(markdown);
  const markdownWithInlineImages = await replaceAsync(
    markdown,
    MARKDOWN_INLINE_IMAGE_PATTERN,
    async (match) => {
      const alt = match[1] ?? '';
      const rawUrl = match[3] ?? match[4] ?? '';
      const title = match[5] ? ` ${match[5]}` : '';
      const rewrittenUrl = await rewriteMarkdownImageUrl(rawUrl, context);
      return `![${alt}](${rewrittenUrl}${title})`;
    },
  );

  return replaceAsync(
    markdownWithInlineImages,
    MARKDOWN_REFERENCE_DEFINITION_PATTERN,
    async (match) => {
      const indent = match[1] ?? '';
      const label = match[2] ?? '';
      const rawUrl = match[4] ?? match[5] ?? '';
      const title = match[6] ? ` ${match[6]}` : '';

      if (!imageReferenceLabels.has(normalizeMarkdownReferenceLabel(label))) {
        return match[0];
      }

      const rewrittenUrl = await rewriteMarkdownImageUrl(rawUrl, context);
      return `${indent}[${label}]: ${rewrittenUrl}${title}`;
    },
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Markdown reference definition 可能也服务普通链接，只有图片引用用到的 label 才能按图片资源策略处理。
 *
 * Code Logic（这个函数做什么）:
 *   扫描 `![alt][label]` 形式并规范化 label；空 label 按 Markdown 约定回退 alt 文本。
 */
function collectMarkdownImageReferenceLabels(markdown: string): Set<string> {
  const labels = new Set<string>();
  let match: RegExpExecArray | null;
  const regex = new RegExp(MARKDOWN_REFERENCE_IMAGE_USE_PATTERN.source, 'g');

  while ((match = regex.exec(markdown)) !== null) {
    const alt = match[1] ?? '';
    const label = match[2] ?? '';
    labels.add(normalizeMarkdownReferenceLabel(label || alt));
  }

  return labels;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Markdown reference label 忽略大小写并折叠空白，预览资源重写需要和 Markdown 解析语义一致。
 *
 * Code Logic（这个函数做什么）:
 *   trim 后把连续空白折叠为单空格并转小写，用作 Set key。
 */
function normalizeMarkdownReferenceLabel(label: string): string {
  return label.trim().replace(/\s+/g, ' ').toLowerCase();
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Markdown 图片 URL 是预览资源入口，必须复用 HTML 预览同一套项目内相对路径策略。
 *
 * Code Logic（这个函数做什么）:
 *   安全 URL 通过 loader 转 data URL；外链、绝对路径、data/blob 或读取失败时返回空字符串。
 */
async function rewriteMarkdownImageUrl(
  rawUrl: string,
  context: HtmlRewriteContext,
): Promise<string> {
  if (!isPreviewAssetUrlEligible(rawUrl)) {
    return '';
  }

  const asset = await context.loadAsset(context.documentPath, rawUrl);
  return asset?.dataUrl ?? '';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   `<link>` 同时承载 stylesheet/icon/preload 等资源链接和 canonical/alternate 等文档关系，不能全部置空或内联。
 *
 * Code Logic（这个函数做什么）:
 *   非 link 资源属性一律重写；link href 仅在 rel 属于可能触发资源加载的关系时重写。
 */
function shouldRewriteResourceAttribute(
  tagName: string,
  attributeName: string,
  tagSource: string,
): boolean {
  if (tagName !== 'link' || attributeName !== 'href') return true;
  const rel = readAttributeValue(tagSource, 'rel')?.toLowerCase() ?? '';
  const relTokens = rel.split(/\s+/).filter(Boolean);
  return relTokens.some((token) => REWRITABLE_LINK_RELS.has(token));
}

/**
 * Business Logic（为什么需要这个函数）:
 *   `<style>` 里的背景图和字体同样属于 HTML 预览资源，不能只处理外部 CSS 文件。
 *
 * Code Logic（这个函数做什么）:
 *   异步替换每个 style block 的内容，保持原始 style 标签属性不变。
 */
async function rewriteInlineStyleBlocks(html: string, context: HtmlRewriteContext): Promise<string> {
  return replaceAsync(html, STYLE_BLOCK_PATTERN, async (match) => {
    const attributes = match[1] ?? '';
    const css = match[2] ?? '';
    const rewrittenCss = await rewriteCssAssetUrls(css, context.documentPath, context.loadAsset);
    return `<style${attributes}>${rewrittenCss}</style>`;
  });
}

/**
 * Business Logic（为什么需要这个函数）:
 *   HTML 标签属性可能使用单引号、双引号或无引号写法，资源改写需要尽量保留原标签其他内容。
 *
 * Code Logic（这个函数做什么）:
 *   针对指定属性执行异步替换；srcset 走候选集解析，普通 URL 走单资源解析。
 */
async function rewriteHtmlAttribute(
  tagSource: string,
  tagName: string,
  attributeName: string,
  context: HtmlRewriteContext,
): Promise<string> {
  const pattern = new RegExp(
    `(\\s${escapeRegExp(attributeName)})(\\s*=\\s*)(?:"([^"]*)"|'([^']*)'|([^\\s"'=<>` + '`' + `]+))`,
    'gi',
  );

  return replaceAsync(tagSource, pattern, async (match) => {
    const prefix = match[1] ?? '';
    const assignment = match[2] ?? '=';
    const quote = match[3] !== undefined ? '"' : match[4] !== undefined ? "'" : '"';
    const value = match[3] ?? match[4] ?? match[5] ?? '';
    const rewrittenValue =
      attributeName === 'srcset'
        ? await rewriteSrcset(value, context)
        : await rewriteSingleAssetValue(tagName, attributeName, value, tagSource, context);

    return `${prefix}${assignment}${quote}${rewrittenValue}${quote}`;
  });
}

/**
 * Business Logic（为什么需要这个函数）:
 *   link stylesheet 资源需要把 CSS 内部相对 url() 一并内联，而普通图片/媒体资源只需使用后端 data URL。
 *
 * Code Logic（这个函数做什么）:
 *   不允许的 URL 直接返回空字符串；CSS 资源读取 text 后二次重写并重新编码为 data URL。
 */
async function rewriteSingleAssetValue(
  tagName: string,
  attributeName: string,
  value: string,
  tagSource: string,
  context: HtmlRewriteContext,
): Promise<string> {
  if (!isPreviewAssetUrlEligible(value)) {
    return '';
  }

  const asset = await context.loadAsset(context.documentPath, value);
  if (!asset) {
    return '';
  }

  if (isStylesheetAsset(tagName, attributeName, value, tagSource, asset)) {
    const css = asset.text ?? '';
    const rewrittenCss = await rewriteCssAssetUrls(css, asset.path, context.loadAsset);
    return textToDataUrl(rewrittenCss, 'text/css');
  }

  return asset.dataUrl;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   响应式图片会在 srcset 中携带多个候选资源，每个候选都必须独立安全解析。
 *
 * Code Logic（这个函数做什么）:
 *   逐项解析 srcset candidate 的 URL 和描述符；不安全或加载失败的候选会被丢弃。
 */
async function rewriteSrcset(value: string, context: HtmlRewriteContext): Promise<string> {
  const rewrittenCandidates: string[] = [];
  for (const candidate of value.split(',')) {
    const trimmed = candidate.trim();
    if (!trimmed) continue;

    const [url = '', ...descriptors] = trimmed.split(SRCSET_SEPARATOR_PATTERN);
    if (!isPreviewAssetUrlEligible(url)) continue;

    const asset = await context.loadAsset(context.documentPath, url);
    if (!asset) continue;

    rewrittenCandidates.push([asset.dataUrl, ...descriptors].filter(Boolean).join(' '));
  }
  return rewrittenCandidates.join(', ');
}

/**
 * Business Logic（为什么需要这个函数）:
 *   只有 stylesheet link 或 text/css 资源才需要 CSS 二次重写，避免把图片等二进制资源误按文本处理。
 *
 * Code Logic（这个函数做什么）:
 *   根据标签/属性、rel 值、MIME 和路径扩展名综合判断资源是否为 CSS。
 */
function isStylesheetAsset(
  tagName: string,
  attributeName: string,
  value: string,
  tagSource: string,
  asset: WorkbenchHtmlAsset,
): boolean {
  if (tagName !== 'link' || attributeName !== 'href') return false;
  const rel = readAttributeValue(tagSource, 'rel')?.toLowerCase() ?? '';
  return rel.split(/\s+/).includes('stylesheet') || asset.mime === 'text/css' || value.toLowerCase().includes('.css');
}

/**
 * Business Logic（为什么需要这个函数）:
 *   判断 link rel 时需要从原始标签读取属性，不能依赖完整 DOM parser 才能让 Node 脚本测试运行。
 *
 * Code Logic（这个函数做什么）:
 *   使用局部属性正则提取指定属性值，支持单引号、双引号和无引号写法。
 */
function readAttributeValue(tagSource: string, attributeName: string): string | null {
  const pattern = new RegExp(
    `\\s${escapeRegExp(attributeName)}\\s*=\\s*(?:"([^"]*)"|'([^']*)'|([^\\s"'=<>` + '`' + `]+))`,
    'i',
  );
  const match = pattern.exec(tagSource);
  return match ? match[1] ?? match[2] ?? match[3] ?? null : null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   前端重写外部 CSS 后需要重新生成 text/css data URL，交给 iframe 作为 stylesheet href。
 *
 * Code Logic（这个函数做什么）:
 *   使用 TextEncoder 转 UTF-8 字节，再 base64 编码为 data URL。
 */
function textToDataUrl(text: string, mime: string): string {
  const bytes = new TextEncoder().encode(text);
  return `data:${mime};base64,${encodeBase64(bytes)}`;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   浏览器和 Node 测试环境都需要稳定的 base64 编码能力。
 *
 * Code Logic（这个函数做什么）:
 *   优先使用 btoa；若运行时没有 btoa，则使用 Node Buffer 兼容路径。
 */
function encodeBase64(bytes: Uint8Array): string {
  if (typeof btoa === 'function') {
    let binary = '';
    const chunkSize = 0x8000;
    for (let index = 0; index < bytes.length; index += chunkSize) {
      const chunk = bytes.slice(index, index + chunkSize);
      binary += String.fromCharCode(...chunk);
    }
    return btoa(binary);
  }

  const buffer = (globalThis as typeof globalThis & {
    Buffer?: { from: (data: Uint8Array) => { toString: (encoding: string) => string } };
  }).Buffer;
  if (!buffer) {
    return '';
  }
  return buffer.from(bytes).toString('base64');
}

/**
 * Business Logic（为什么需要这个函数）:
 *   异步资源加载不能直接用于 String.replace，需要一个保持原字符串顺序的替换工具。
 *
 * Code Logic（这个函数做什么）:
 *   遍历正则匹配片段，串联未匹配文本和异步 replacement 结果。
 */
async function replaceAsync(
  input: string,
  pattern: RegExp,
  replacer: AsyncMatchReplacer,
): Promise<string> {
  const flags = pattern.flags.includes('g') ? pattern.flags : `${pattern.flags}g`;
  const regex = new RegExp(pattern.source, flags);
  let output = '';
  let lastIndex = 0;
  let match: RegExpExecArray | null;

  while ((match = regex.exec(input)) !== null) {
    output += input.slice(lastIndex, match.index);
    output += await replacer(match);
    lastIndex = regex.lastIndex;
  }

  output += input.slice(lastIndex);
  return output;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   属性名会被插入动态正则，必须转义避免特殊字符影响匹配。
 *
 * Code Logic（这个函数做什么）:
 *   转义正则元字符，返回安全字面量片段。
 */
function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}
