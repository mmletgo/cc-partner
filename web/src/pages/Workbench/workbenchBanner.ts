/**
 * 工作台顶栏标语：本机存储、轻量 Markdown 与字号拟合。
 *
 * Business Logic（为什么需要这个模块）:
 *   标语是本机全局、不进局域网同步的短文；解析与字号必须可单测，
 *   不能绑在 React 组件或后端 config 上。
 *
 * Code Logic（这个模块做什么）:
 *   localStorage 读写 fail-closed；限制 UTF-16 长度；只识别粗体/斜体/
 *   删除线/行内代码/http(s) 链接/换行；按容器二分最大字号。
 */

export const BANNER_STORAGE_KEY = 'cp-workbench-banner';
export const BANNER_CHANGE_EVENT = 'cp-workbench-banner-change';
export const BANNER_SCHEMA_VERSION = 1 as const;
export const BANNER_MAX_CHARS = 280;
export const BANNER_MIN_FONT_PX = 11;
export const BANNER_MAX_FONT_PX = 64;
export const BANNER_LINE_HEIGHT = 1.2;
export const BANNER_SAVE_DEBOUNCE_MS = 400;

export interface WorkbenchBannerRecord {
  version: typeof BANNER_SCHEMA_VERSION;
  markdown: string;
  updatedAt: number;
}

export type BannerInline =
  | { type: 'text'; value: string }
  | { type: 'strong'; value: string }
  | { type: 'em'; value: string }
  | { type: 'del'; value: string }
  | { type: 'code'; value: string }
  | { type: 'link'; text: string; href: string }
  | { type: 'break' };

/**
 * Business Logic（为什么需要这个函数）:
 *   标语区很扁，超长正文会缩成不可读的蚂蚁字。
 *
 * Code Logic（这个函数做什么）:
 *   按 UTF-16 单位截到 BANNER_MAX_CHARS。
 */
export function clampBannerMarkdown(markdown: string): string {
  if (markdown.length <= BANNER_MAX_CHARS) return markdown;
  return markdown.slice(0, BANNER_MAX_CHARS);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   预览里的链接必须可点，但不能执行 javascript: 或相对协议。
 *
 * Code Logic（这个函数做什么）:
 *   仅接受 http/https 绝对 URL，空白即拒绝。
 */
export function isSafeHttpUrl(href: string): boolean {
  if (href.length === 0 || /[\s]/.test(href)) return false;
  try {
    const url = new URL(href);
    return url.protocol === 'http:' || url.protocol === 'https:';
  } catch {
    return false;
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   启动与跨窗口同步需要读出上次写下的标语，坏数据不能炸顶栏。
 *
 * Code Logic（这个函数做什么）:
 *   解析 version=1 JSON；缺字段/非法/超长按空或截断处理。
 */
export function readWorkbenchBanner(): string {
  if (typeof window === 'undefined') return '';
  try {
    const raw = window.localStorage.getItem(BANNER_STORAGE_KEY);
    if (raw == null || raw === '') return '';
    const parsed: unknown = JSON.parse(raw);
    if (!isRecord(parsed)) return '';
    if (parsed.version !== BANNER_SCHEMA_VERSION) return '';
    if (typeof parsed.markdown !== 'string') return '';
    return clampBannerMarkdown(parsed.markdown);
  } catch {
    return '';
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   用户编辑后要立刻落到本机，并让同窗口其它订阅者对齐。
 *
 * Code Logic（这个函数做什么）:
 *   clamp 后写 localStorage，派发 BANNER_CHANGE_EVENT；写失败吞掉。
 */
export function writeWorkbenchBanner(markdown: string): WorkbenchBannerRecord {
  const record: WorkbenchBannerRecord = {
    version: BANNER_SCHEMA_VERSION,
    markdown: clampBannerMarkdown(markdown),
    updatedAt: Date.now(),
  };
  if (typeof window !== 'undefined') {
    try {
      window.localStorage.setItem(BANNER_STORAGE_KEY, JSON.stringify(record));
      window.dispatchEvent(
        new CustomEvent<string>(BANNER_CHANGE_EVENT, { detail: record.markdown }),
      );
    } catch {
      // quota / 隐私模式：预览仍用内存稿
    }
  }
  return record;
}

interface MarkMatch {
  node: BannerInline;
  next: number;
}

function takeWrapped(
  line: string,
  start: number,
  marker: string,
  type: 'strong' | 'em' | 'del' | 'code',
): MarkMatch | null {
  if (!line.startsWith(marker, start)) return null;
  const contentStart = start + marker.length;
  const end = line.indexOf(marker, contentStart);
  if (end <= contentStart) return null;
  return {
    node: { type, value: line.slice(contentStart, end) },
    next: end + marker.length,
  };
}

function takeLink(line: string, start: number): MarkMatch | null {
  if (line[start] !== '[') return null;
  const closeLabel = line.indexOf('](', start + 1);
  if (closeLabel <= start + 1) return null;
  const closeHref = line.indexOf(')', closeLabel + 2);
  if (closeHref <= closeLabel + 2) return null;
  const text = line.slice(start + 1, closeLabel);
  const href = line.slice(closeLabel + 2, closeHref);
  if (text.length === 0 || !isSafeHttpUrl(href)) return null;
  return {
    node: { type: 'link', text, href },
    next: closeHref + 1,
  };
}

function takeMark(line: string, start: number): MarkMatch | null {
  return (
    takeWrapped(line, start, '~~', 'del') ??
    takeWrapped(line, start, '**', 'strong') ??
    takeWrapped(line, start, '`', 'code') ??
    takeWrapped(line, start, '*', 'em') ??
    takeLink(line, start)
  );
}

function parseInline(line: string): BannerInline[] {
  const nodes: BannerInline[] = [];
  let i = 0;
  let text = '';
  const flushText = (): void => {
    if (text.length === 0) return;
    nodes.push({ type: 'text', value: text });
    text = '';
  };
  while (i < line.length) {
    const marked = takeMark(line, i);
    if (marked) {
      flushText();
      nodes.push(marked.node);
      i = marked.next;
      continue;
    }
    text += line[i];
    i += 1;
  }
  flushText();
  return nodes;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   顶栏只渲染标语向语法；标题/列表/HTML 必须当普通字，避免撑破扁区域。
 *
 * Code Logic（这个函数做什么）:
 *   按行切分，行内识别 ~~ ** ` * 与安全链接；未闭合标记保留原文。
 */
export function parseBannerMarkdown(source: string): BannerInline[] {
  const lines = source.replace(/\r\n/g, '\n').split('\n');
  const nodes: BannerInline[] = [];
  lines.forEach((line, index) => {
    nodes.push(...parseInline(line));
    if (index < lines.length - 1) {
      nodes.push({ type: 'break' });
    }
  });
  return nodes;
}

export interface BannerFontMeasure {
  width: number;
  height: number;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   标语必须整段可见并尽量撑满红框，短句大、长句小，且不拉字距。
 *
 * Code Logic（这个函数做什么）:
 *   在 [min, min(max, 高度/行高)] 上二分最大可放下字号；量不下则回 min。
 */
export function fitBannerFontSize(options: {
  maxWidth: number;
  maxHeight: number;
  measure: (fontPx: number) => BannerFontMeasure;
  minPx?: number;
  maxPx?: number;
}): number {
  const minPx = options.minPx ?? BANNER_MIN_FONT_PX;
  const requestedMax = options.maxPx ?? BANNER_MAX_FONT_PX;
  if (options.maxWidth <= 0 || options.maxHeight <= 0) return minPx;
  const heightCap = Math.max(minPx, Math.floor(options.maxHeight / BANNER_LINE_HEIGHT));
  const maxPx = Math.max(minPx, Math.min(requestedMax, heightCap));

  const fits = (px: number): boolean => {
    const box = options.measure(px);
    return box.width <= options.maxWidth && box.height <= options.maxHeight;
  };

  if (!fits(minPx)) return minPx;
  if (fits(maxPx)) return maxPx;

  let lo = minPx;
  let hi = maxPx;
  let best = minPx;
  while (lo <= hi) {
    const mid = Math.floor((lo + hi) / 2);
    if (fits(mid)) {
      best = mid;
      lo = mid + 1;
    } else {
      hi = mid - 1;
    }
  }
  return best;
}
