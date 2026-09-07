/**
 * screenshotOverlayInput — 截图 Overlay 键盘与工具条定位纯规则
 *
 * Business Logic: 框选完成后工具条可能被屏幕边缘裁切，用户需要不依赖按钮也能确认；
 *   回车确认、Esc 取消，工具条必须夹在视口内。
 *
 * Code Logic: 键盘与定位都不读 DOM，方便单测覆盖边缘选区与按键策略。
 */

export type ScreenshotOverlayMode = 'idle' | 'selecting' | 'editing';

export type ScreenshotOverlayKeyAction = 'cancel' | 'confirm' | 'ignore';

/** 工具条固有宽度（图标按钮固定，略放大避免贴右缘裁切） */
export const SCREENSHOT_TOOLBAR_WIDTH = 360;

/** 工具条固有高度（padding 6 + 按钮 30 + padding 6，外加一点余量） */
export const SCREENSHOT_TOOLBAR_HEIGHT = 44;

/** 工具条与选区边缘的间距 */
export const SCREENSHOT_TOOLBAR_GAP = 8;

/**
 * Business Logic（为什么需要）:
 *   选区贴边时确认按钮可能看不见，回车必须等价于点确认；Esc 始终取消。
 *
 * Code Logic（做什么）:
 *   Escape → cancel；editing 且非 composing/repeat 的 Enter → confirm；其余 ignore。
 */
export function resolveScreenshotOverlayKey(
  event: { key: string; isComposing?: boolean; repeat?: boolean },
  mode: ScreenshotOverlayMode,
): ScreenshotOverlayKeyAction {
  if (event.key === 'Escape') return 'cancel';
  if (event.key !== 'Enter' || event.isComposing || event.repeat) return 'ignore';
  if (mode !== 'editing') return 'ignore';
  return 'confirm';
}

interface ScreenshotSelectionBox {
  x: number;
  y: number;
  w: number;
  h: number;
}

interface ScreenshotViewportSize {
  width: number;
  height: number;
}

interface ScreenshotToolbarSize {
  width: number;
  height: number;
}

/**
 * Business Logic（为什么需要）:
 *   工具条默认居中于选区，但贴左/右/底边时必须整体挪进视口，避免确认按钮被 overflow 裁掉。
 *
 * Code Logic（做什么）:
 *   水平以选区中心对齐后 clamp 到 [0, viewportW-toolbarW]；
 *   下方放得下就放下方，否则翻到上方，两边都不够则贴视口底。
 */
export function computeScreenshotToolbarPosition(
  selection: ScreenshotSelectionBox,
  viewport: ScreenshotViewportSize,
  toolbar: ScreenshotToolbarSize = {
    width: SCREENSHOT_TOOLBAR_WIDTH,
    height: SCREENSHOT_TOOLBAR_HEIGHT,
  },
): { left: number; top: number } {
  const maxLeft = Math.max(0, viewport.width - toolbar.width);
  const centered = selection.x + selection.w / 2 - toolbar.width / 2;
  const left = Math.min(maxLeft, Math.max(0, centered));

  const below = selection.y + selection.h + SCREENSHOT_TOOLBAR_GAP;
  const above = selection.y - toolbar.height - SCREENSHOT_TOOLBAR_GAP;
  const top =
    below + toolbar.height <= viewport.height
      ? below
      : above >= 0
        ? above
        : Math.max(0, viewport.height - toolbar.height);

  return { left, top };
}
