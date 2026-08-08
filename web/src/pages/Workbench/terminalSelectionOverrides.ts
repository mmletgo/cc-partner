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
 *   工作台终端经常跑 Claude Code / tmux TUI，它们会开启 xterm mouse tracking。
 *   xterm 默认行为：
 *   1. mouse protocol 激活时 selectionService.disable() —— 直接 clearSelection 并禁止拖选；
 *   2. mouse report 以 wasUserInput=true 上报，onUserInput 再次 clearSelection。
 *   结果是“刚选中就立刻消失、无法复制”。Workbench 优先保证文本复制，
 *   分栏点击走我们自己的 pointer 几何判定，不依赖 PTY mouse mode。
 *
 * Code Logic（这个函数做什么）:
 *   open() 之后 patch selectionService：
 *   - shouldForceSelection 恒 true：mousedown 不向 PTY 发 mouse report；
 *   - disable 改 no-op：CSI mouse-mode 序列不再清选区/禁选；
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

  service.shouldForceSelection = (): boolean => true;
  service.disable = (): void => {
    // intentionally no-op: keep text selection available for copy
  };
  service.enable();

  return () => {
    service.shouldForceSelection = originalShouldForceSelection;
    service.disable = originalDisable;
  };
}
