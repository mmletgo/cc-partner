import type { Terminal } from '@xterm/xterm';

/**
 * xterm 内部 selection service 的最小面（仅本文件 monkey-patch 使用）。
 * 公开 API 无法关闭 mouse-mode 对选区的禁用，必须触及 _core._selectionService。
 */
interface XtermSelectionServiceLike {
  shouldForceSelection: (event: MouseEvent) => boolean;
  disable: () => void;
  enable: () => void;
}

interface XtermCoreLike {
  _selectionService?: XtermSelectionServiceLike;
}

interface XtermPublicLike {
  _core?: XtermCoreLike;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Grok Build / Claude Code 等 TUI 开启 mouse tracking 后，普通点击必须变成 SGR
 *   mouse report 才能点按钮。与此同时，用户仍需要一种不发给 PTY 的选字手势。
 *   标准终端约定：Option（macOS，xterm 原应）或 Shift（跨平台）强制选字。
 *
 * Code Logic（这个函数做什么）:
 *   Shift 一律强制选字；其余委托 xterm 原 shouldForceSelection（含
 *   macOptionClickForcesSelection + Option）。
 */
export function shouldForceWorkbenchTerminalSelection(
  event: Pick<MouseEvent, 'shiftKey'>,
  originalShouldForce: (event: MouseEvent) => boolean,
): boolean {
  if (event.shiftKey) return true;
  return originalShouldForce(event as MouseEvent);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   工作台终端经常跑 Claude Code / Grok / tmux TUI，它们会开启 xterm mouse tracking。
 *   xterm 默认行为：
 *   1. mouse protocol 激活时 selectionService.disable() —— 直接 clearSelection 并禁止拖选；
 *   2. 普通点击发 mouse report（wasUserInput=true），onUserInput 会再清一次选区。
 *   复制仍必须可用，但不能再把所有 mousedown 都当成选字，否则 TUI 按钮永远点不到。
 *
 * Code Logic（这个函数做什么）:
 *   open() 之后 patch selectionService：
 *   - shouldForceSelection：仅 Shift / 原 Option 手势强制选字，普通点击发给 TUI；
 *   - disable 改 no-op：CSI mouse-mode 序列不再清选区/禁选，Option/Shift 拖选才能留下；
 *   - 主动 enable() 一次，覆盖 attach 时已激活的 mouse protocol。
 *   返回 restore 函数供 dispose 时还原。
 */
export function installWorkbenchTerminalSelectionOverrides(terminal: Terminal): () => void {
  const core = (terminal as unknown as XtermPublicLike)._core;
  const service = core?._selectionService;
  if (!service) {
    return () => undefined;
  }

  const originalShouldForceSelection = service.shouldForceSelection.bind(service);
  const originalDisable = service.disable.bind(service);

  service.shouldForceSelection = (event: MouseEvent): boolean =>
    shouldForceWorkbenchTerminalSelection(event, originalShouldForceSelection);
  service.disable = (): void => {
    // intentionally no-op: keep Option/Shift text selection available for copy
  };
  service.enable();

  return () => {
    service.shouldForceSelection = originalShouldForceSelection;
    service.disable = originalDisable;
  };
}
