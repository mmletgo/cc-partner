/**
 * 工作台终端把文件交给 Agent 的纯函数。
 *
 * Business Logic（为什么需要这个模块）:
 *   拖入必须命中终端面板才交给当前 Agent；粘贴时图片仍走 paste-image，非图片才走附件通道。
 *   浏览器 File 没有绝对路径，粘贴只能送文件名+字节，拖入才有 Tauri 原生路径。
 *
 * Code Logic（这个模块做什么）:
 *   命中测试、非图片 File 提取、file:// URI 解析、体积上限、blob 编码。
 */

/** 与后端 `MAX_AGENT_ATTACH_BYTES` 对齐的单次总量上限。 */
export const MAX_TERMINAL_ATTACH_BYTES = 20 * 1024 * 1024;

export interface CssRect {
  left: number;
  top: number;
  right: number;
  bottom: number;
}

export interface AttachBlob {
  relativePath: string;
  contentBase64: string;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Tauri drop 坐标是整窗的，必须只在落在终端面板内时才交给 Agent。
 *
 * Code Logic（这个函数做什么）:
 *   闭区间命中 CSS 矩形。
 */
export function logicalPointHitsRect(x: number, y: number, rect: CssRect): boolean {
  return x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   onDragDropEvent 给物理像素，getBoundingClientRect 是 CSS 像素。
 *
 * Code Logic（这个函数做什么）:
 *   用 scaleFactor 除掉；非法 scale 回落 1。
 */
export function physicalDropPointToCss(
  position: { x: number; y: number },
  scaleFactor: number,
): { x: number; y: number } {
  const scale = Number.isFinite(scaleFactor) && scaleFactor > 0 ? scaleFactor : 1;
  return { x: position.x / scale, y: position.y / scale };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   粘贴可能同时带图和文件；图优先走 paste-image，避免 PDF 把截图挤掉。
 *
 * Code Logic（这个函数做什么）:
 *   有 image/* 则返回空；否则收集非图片 File。
 */
export function clipboardEventNonImageFiles(event: ClipboardEvent): File[] {
  const files = event.clipboardData?.files;
  if (!files || files.length === 0) return [];
  const all = Array.from(files);
  if (all.some((file) => file.type.startsWith('image/'))) return [];
  return all.filter((file) => !file.type.startsWith('image/'));
}

/**
 * Business Logic（为什么需要这个函数）:
 *   部分应用粘贴只给 text/uri-list 的 file://，没有 File 对象。
 *
 * Code Logic（这个函数做什么）:
 *   跳过注释行，解码 file: URL 为本地路径。
 */
export function parseFileUriList(raw: string): string[] {
  const paths: string[] = [];
  for (const line of raw.split(/\r?\n/)) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith('#')) continue;
    try {
      const url = new URL(trimmed);
      if (url.protocol !== 'file:') continue;
      let path = decodeURIComponent(url.pathname);
      if (/^\/[A-Za-z]:\//.test(path)) {
        path = path.slice(1);
      }
      if (path) paths.push(path);
    } catch {
      // 非法 URI 忽略。
    }
  }
  return paths;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   blob 只能用文件名落临时目录，必须拒绝路径穿越。
 *
 * Code Logic（这个函数做什么）:
 *   取 basename，丢掉空、`.`、`..` 和分隔符。
 */
export function safeAttachFileName(name: string): string | null {
  const base = name.replace(/\\/g, '/').split('/').pop()?.trim() ?? '';
  if (!base || base === '.' || base === '..' || base.includes('\0')) return null;
  return base;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   单次交给 Agent 的总量必须在发后端前截住，避免撑爆 32 MiB JSON。
 *
 * Code Logic（这个函数做什么）:
 *   累加 File.size，超过上限返回 false。
 */
export function attachFilesWithinLimit(files: Array<{ size: number }>, maxBytes = MAX_TERMINAL_ATTACH_BYTES): boolean {
  let total = 0;
  for (const file of files) {
    total += Math.max(0, file.size);
    if (total > maxBytes) return false;
  }
  return true;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   粘贴 File 没有绝对路径，sidecar 需要文件名 + STANDARD base64。
 *
 * Code Logic（这个函数做什么）:
 *   FileReader data URL 去掉前缀；文件名走 safeAttachFileName。
 */
export async function filesToAttachBlobs(files: File[]): Promise<AttachBlob[]> {
  const blobs: AttachBlob[] = [];
  for (const file of files) {
    const relativePath = safeAttachFileName(file.name);
    if (!relativePath) {
      throw new Error('文件名非法');
    }
    blobs.push({
      relativePath,
      contentBase64: await fileToBase64(file),
    });
  }
  return blobs;
}

function fileToBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => {
      if (typeof reader.result !== 'string') {
        reject(new Error('无法读取文件'));
        return;
      }
      const comma = reader.result.indexOf(',');
      resolve(comma >= 0 ? reader.result.slice(comma + 1) : reader.result);
    };
    reader.onerror = () => reject(new Error('无法读取文件'));
    reader.readAsDataURL(file);
  });
}
