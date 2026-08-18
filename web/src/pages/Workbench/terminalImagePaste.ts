/**
 * 工作台终端图片粘贴纯函数。
 *
 * Business Logic（为什么需要这个模块）:
 *   Claude Code / Grok 等 Agent TUI 从 CLI 所在机器的 OS 剪贴板读图。xterm 默认只粘贴文字，
 *   远端会话再把 Ctrl+V 转发给对端会读空剪贴板。必须先识别剪贴板里的图片，再交给后端写 owning device。
 *
 * Code Logic（这个模块做什么）:
 *   从 ClipboardEvent 取出 image file、转 PNG data URL、识别 Ctrl+V（不含 Cmd+V）。
 */

/** 与后端 `MAX_CLIPBOARD_IMAGE_BYTES` 对齐的解码前文件上限。 */
export const MAX_TERMINAL_PASTE_IMAGE_BYTES = 8 * 1024 * 1024;

/**
 * Business Logic（为什么需要这个函数）:
 *   paste 事件可能同时带 text/plain 与 image；Agent 图片粘贴应优先图，避免 xterm 把空文本当成功粘贴。
 *
 * Code Logic（这个函数做什么）:
 *   先扫 `clipboardData.items` 的 image/* file，再扫 `files`。
 */
export function clipboardEventImageFile(event: ClipboardEvent): File | null {
  const items = event.clipboardData?.items;
  if (items) {
    for (const item of Array.from(items)) {
      if (item.kind === 'file' && item.type.startsWith('image/')) {
        const file = item.getAsFile();
        if (file) return file;
      }
    }
  }
  const files = event.clipboardData?.files;
  if (files) {
    for (const file of Array.from(files)) {
      if (file.type.startsWith('image/')) return file;
    }
  }
  return null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   后端剪贴板写入按 PNG/JPEG data URL 解码；前端统一成 PNG，避免 webp 等格式在 Rust image crate 未启用时失败。
 *
 * Code Logic（这个函数做什么）:
 *   PNG 直接 FileReader；其它类型用 canvas 再导出 PNG。超限抛错。
 */
export async function fileToPngDataUrl(file: File): Promise<string> {
  if (file.size > MAX_TERMINAL_PASTE_IMAGE_BYTES) {
    throw new Error('粘贴图片过大');
  }
  if (file.type === 'image/png' || file.type === '') {
    return readBlobAsDataUrl(file);
  }
  if (typeof createImageBitmap !== 'function') {
    return readBlobAsDataUrl(file);
  }
  const bitmap = await createImageBitmap(file);
  try {
    const canvas = document.createElement('canvas');
    canvas.width = bitmap.width;
    canvas.height = bitmap.height;
    const context = canvas.getContext('2d');
    if (!context) {
      throw new Error('无法编码粘贴图片');
    }
    context.drawImage(bitmap, 0, 0);
    const dataUrl = canvas.toDataURL('image/png');
    if (!dataUrl.startsWith('data:image/png;base64,')) {
      throw new Error('无法编码粘贴图片');
    }
    return dataUrl;
  } finally {
    bitmap.close();
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   macOS Cmd+V 走 paste 事件；Ctrl+V（含 Windows/Linux 以及 macOS 给 Agent 的图片快捷键）不带 meta。
 *
 * Code Logic（这个函数做什么）:
 *   keydown + ctrl + v，排除 meta/alt/shift。
 */
export function isCtrlVPasteKey(event: KeyboardEvent): boolean {
  return (
    event.type === 'keydown' &&
    event.ctrlKey &&
    !event.metaKey &&
    !event.altKey &&
    !event.shiftKey &&
    event.key.toLowerCase() === 'v'
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   FileReader 把 Blob 读成 data URL，供 PNG 直通路径与测试使用。
 *
 * Code Logic（这个函数做什么）:
 *   包装 FileReader.readAsDataURL，失败 reject。
 */
function readBlobAsDataUrl(blob: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => {
      if (typeof reader.result === 'string') {
        resolve(reader.result);
        return;
      }
      reject(new Error('无法读取粘贴图片'));
    };
    reader.onerror = () => {
      reject(new Error('无法读取粘贴图片'));
    };
    reader.readAsDataURL(blob);
  });
}
