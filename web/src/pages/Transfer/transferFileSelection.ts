/**
 * Transfer 原生路径选择适配器。
 *
 * Business Logic（为什么需要这个模块）:
 *   桌面文件传输必须拿到三平台可供 Rust 打开的绝对路径；浏览器 `<input type="file">`
 *   的 File 对象不保证路径。浏览用 native dialog，拖放用 webview drag-drop 事件。
 *
 * Code Logic（这个模块做什么）:
 *   - pickTransferFile：动态 import plugin-dialog，open({ multiple:false, directory:false })，
 *     仅接受 string 路径；取消或非 string 返回 null；路径不改写分隔符/不 decode URI。
 *   - subscribeTransferFileDrops：仅在 Tauri internals 可用时注册 onDragDropEvent，
 *     drop.paths 原样回调；非 Tauri 环境返回 no-op unsubscribe，保证 Playwright 稳定。
 */

type TauriInternalsWindow = Window & {
  __TAURI_INTERNALS__?: {
    transformCallback?: (...args: unknown[]) => unknown;
  };
};

/**
 * Business Logic（为什么需要这个函数）:
 *   Playwright/Vite 普通浏览器没有 Tauri event internals，注册 drag listener 会白屏。
 *
 * Code Logic（这个函数做什么）:
 *   检测 window.__TAURI_INTERNALS__.transformCallback 是否为函数。
 */
function canUseTauriNativeApis(): boolean {
  if (typeof window === 'undefined') return false;
  const internals = (window as TauriInternalsWindow).__TAURI_INTERNALS__;
  return typeof internals?.transformCallback === 'function';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   用户点击“浏览”时需要选择本机单个文件路径发给局域网设备。
 *
 * Code Logic（这个函数做什么）:
 *   动态 import `@tauri-apps/plugin-dialog` 的 open，固定 multiple/directory 为 false；
 *   返回 string 路径或 null（取消/非 string）；路径按不透明 UTF-8 原样返回。
 */
export async function pickTransferFile(): Promise<string | null> {
  const { open } = await import('@tauri-apps/plugin-dialog');
  const selected = await open({ multiple: false, directory: false });
  if (typeof selected === 'string') {
    return selected;
  }
  return null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   用户把文件拖进传输页 dropzone 时，需要拿到原生绝对路径列表供发送。
 *
 * Code Logic（这个函数做什么）:
 *   非 Tauri 环境直接返回 no-op unsubscribe；否则动态 import webview，
 *   订阅 onDragDropEvent，仅在 type==='drop' 时把 paths 原样传给 onPaths。
 */
export async function subscribeTransferFileDrops(
  onPaths: (paths: string[]) => void,
): Promise<() => void> {
  if (!canUseTauriNativeApis()) {
    return () => undefined;
  }

  const { getCurrentWebview } = await import('@tauri-apps/api/webview');
  const unlisten = await getCurrentWebview().onDragDropEvent((event) => {
    if (event.payload.type !== 'drop') return;
    onPaths(event.payload.paths);
  });
  return unlisten;
}
