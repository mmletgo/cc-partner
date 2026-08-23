/**
 * 移动端终端复制的剪贴板写入封装。
 *
 * Business Logic（为什么需要这个模块）:
 *   `/mobile` 走局域网 HTTP，不是安全上下文，`navigator.clipboard.writeText` 经常抛错；
 *   终端选区复制仍要把原文写入系统剪贴板，失败时用隐藏 textarea + execCommand 兜底。
 *
 * Code Logic（这个模块做什么）:
 *   可注入 clipboardWriteText / execCommand / document 的薄包装；默认接到浏览器全局 API。
 */

export type WriteClipboardTextResult =
  | { ok: true; method: 'clipboard' | 'execCommand' }
  | { ok: false; reason: 'empty' | 'failed' };

export interface WriteClipboardTextDeps {
  clipboardWriteText?: (text: string) => Promise<void>;
  execCommand?: (commandId: string) => boolean;
  document?: {
    body: {
      appendChild: (node: HTMLTextAreaElement) => void;
      removeChild: (node: HTMLTextAreaElement) => void;
    };
    createElement: (tag: 'textarea') => HTMLTextAreaElement;
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   注入测试替身时不得回落到真实 navigator/document，避免非安全上下文或 jsdom 干扰断言。
 *
 * Code Logic（这个函数做什么）:
 *   deps 缺省时取 navigator.clipboard.writeText；传入 deps 则只用其 clipboardWriteText 字段。
 */
function resolveClipboardWriteText(
  deps: WriteClipboardTextDeps | undefined,
): ((text: string) => Promise<void>) | undefined {
  if (deps) {
    return deps.clipboardWriteText;
  }
  const clipboard = globalThis.navigator?.clipboard;
  const writeText = clipboard?.writeText;
  if (typeof writeText !== 'function') {
    return undefined;
  }
  return (text: string) => writeText.call(clipboard, text);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   execCommand('copy') 是 clipboard API 失败后的唯一兜底，测试必须能替换实现。
 *
 * Code Logic（这个函数做什么）:
 *   deps 缺省时绑定 document.execCommand；传入 deps 则只用其 execCommand 字段。
 */
function resolveExecCommand(
  deps: WriteClipboardTextDeps | undefined,
): ((commandId: string) => boolean) | undefined {
  if (deps) {
    return deps.execCommand;
  }
  if (typeof document === 'undefined' || typeof document.execCommand !== 'function') {
    return undefined;
  }
  return (commandId: string) => document.execCommand(commandId);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   execCommand 路径需要临时 textarea，测试用最小 fake document 即可，不能碰真实 DOM。
 *
 * Code Logic（这个函数做什么）:
 *   deps 缺省时包装真实 document.createElement('textarea')；传入 deps 则只用其 document 字段。
 */
function resolveDocument(
  deps: WriteClipboardTextDeps | undefined,
): WriteClipboardTextDeps['document'] | undefined {
  if (deps) {
    return deps.document;
  }
  if (typeof document === 'undefined') {
    return undefined;
  }
  return {
    body: document.body,
    createElement: (tag: 'textarea') => document.createElement(tag),
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端终端选区复制要写入手机剪贴板且不写 PTY；非安全上下文下 clipboard API 常失败。
 *
 * Code Logic（这个函数做什么）:
 *   空串直接 empty；优先 clipboardWriteText，抛错/拒绝/缺失则建临时 textarea 后 execCommand('copy')；
 *   两路都没有或失败则 failed。原文原样复制，不做 trim。
 */
export async function writeClipboardText(
  text: string,
  deps?: WriteClipboardTextDeps,
): Promise<WriteClipboardTextResult> {
  if (text.length === 0) {
    return { ok: false, reason: 'empty' };
  }

  const clipboardWriteText = resolveClipboardWriteText(deps);
  const execCommand = resolveExecCommand(deps);
  const doc = resolveDocument(deps);

  if (clipboardWriteText) {
    try {
      await clipboardWriteText(text);
      return { ok: true, method: 'clipboard' };
    } catch {
      // 非安全上下文或权限拒绝时走 execCommand 兜底。
    }
  }

  if (execCommand && doc) {
    let textarea: HTMLTextAreaElement | undefined;
    try {
      textarea = doc.createElement('textarea');
      textarea.value = text;
      doc.body.appendChild(textarea);
      textarea.select();
      if (execCommand('copy')) {
        return { ok: true, method: 'execCommand' };
      }
    } catch {
      // 创建节点或 execCommand 失败视为本通道失败。
    } finally {
      if (textarea) {
        try {
          doc.body.removeChild(textarea);
        } catch {
          // 未成功挂上或已被移除时忽略。
        }
      }
    }
  }

  return { ok: false, reason: 'failed' };
}
