/// 终端触控滚动与 SGR 滚轮转发纯函数（对齐 web mobileTerminalTouchScroll.ts / terminalWheel.ts）。
///
/// Business Logic（为什么需要这个模块）:
///   xterm 4.0.0 的 TerminalView 只在 alternate screen 用内置手势转发滚轮，且 SGR button id
///   用了非标准的 68/69（web/xterm.js 权威为 64/65）；normal buffer + mouse tracking（Claude
///   Code 经 tmux passthrough 的真实形态）下完全不转发。移动端必须把单指拖动自行编码成
///   `CSI < 64/65 ; col ; row M` 帧经输入流发给 PTY，普通 buffer 保持内置 scrollback 滚动。
///
/// Code Logic（这个模块做什么）:
///   纯函数：像素→行换算（带余量）、SGR 滚轮帧编码（单帧最多 8 事件）、滚动模式判定、
///   hydration 触发意图累计、尺寸 clamp。全部无 IO，配单测。
library;

/// 单次 touchmove 最多向 PTY 注入的 SGR 滚轮事件数，防止高速滑动打爆输入流（web 同值）。
const int kTerminalTuiWheelEventsCap = 8;

/// 新建会话时实测尺寸的列下限（对齐 web MIN_TERMINAL_COLS）。
const int kMinTerminalCols = 20;

/// 新建会话时实测尺寸的行下限（对齐 web MIN_TERMINAL_ROWS）。
const int kMinTerminalRows = 6;

/// PTY resize/create 尺寸上限（u16），对齐 web clampU16。
const int kMaxTerminalDimension = 65535;

/// 触控滚动累计状态：上次触点 Y 与不足一行的像素余量。
class TouchScrollState {
  const TouchScrollState({required this.lastClientY, this.remainderPx = 0});

  final double lastClientY;
  final double remainderPx;
}

/// 一次 touchmove 的换算结果：本次应滚动/转发的整数行数与新状态。
class TouchScrollUpdate {
  const TouchScrollUpdate({required this.lines, required this.state});

  final int lines;
  final TouchScrollState state;
}

/// 终端一次拖动的滚动处置方式（对齐 web resolveWorkbenchTerminalWheelAction）。
enum TerminalScrollAction {
  /// mouse tracking 未协商且 normal buffer：本地 scrollback 滚动（历史已 hydration 时）。
  localScrollback,

  /// mouse tracking 未协商、normal buffer、尚未用 owner 端 tmux 历史灌过本地 buffer：
  /// 先 hydration，不本地滚动。
  hydrateScrollback,

  /// mouse tracking 已协商（或 alternate screen）：拖动编码为 SGR wheel 帧转发 PTY。
  forwardSgrWheel,
}

/// 业务逻辑：尺寸必须落在 u16 且不小于可用下限，否则后端 PTY resize/create 拒绝或错位。
///
/// Code Logic：非法输入返回 min；四舍五入后 clamp 到 min..65535（对齐 web clampU16）。
int clampTerminalDimension(num value, int min) {
  if (!value.isFinite) {
    return min;
  }
  final rounded = value.round();
  if (rounded < min) {
    return min;
  }
  if (rounded > kMaxTerminalDimension) {
    return kMaxTerminalDimension;
  }
  return rounded;
}

/// 业务逻辑：xterm 行高随 fit 后的 rows 变化，拖动像素必须按当前可见行高换算。
///
/// Code Logic：优先 viewportHeight / rows；不可用时回退 fallbackPx；至少 1px（web 同语义）。
double terminalTouchLineHeight(double viewportHeight, int rows, double fallbackPx) {
  if (viewportHeight.isFinite && viewportHeight > 0 && rows > 0) {
    final h = viewportHeight / rows;
    return h < 1 ? 1 : h;
  }
  if (fallbackPx.isFinite && fallbackPx > 0) {
    return fallbackPx;
  }
  return 1;
}

/// 业务逻辑：手指连续小位移不能丢掉不足一行的像素，否则慢速滑动无响应。
///
/// Code Logic：本次 Y 位移叠加上次余量后换算整数行；正数=向底部，负数=向顶部；
/// 余量保留在状态里（web updateMobileTerminalTouchScroll 同语义）。
TouchScrollUpdate updateTouchScroll(
  TouchScrollState state,
  double clientY,
  double lineHeightPx,
) {
  final safeLineHeight = lineHeightPx.isFinite && lineHeightPx > 0 ? lineHeightPx : 1.0;
  final deltaPx = state.lastClientY - clientY;
  final totalPx = state.remainderPx + deltaPx;
  final lines = (totalPx / safeLineHeight).truncate();
  return TouchScrollUpdate(
    lines: lines,
    state: TouchScrollState(
      lastClientY: clientY,
      remainderPx: totalPx - lines * safeLineHeight,
    ),
  );
}

/// 一次触点换算出的 SGR wheel 1-based 字符格坐标。
class TerminalWheelCell {
  const TerminalWheelCell({required this.col, required this.row});

  final int col;
  final int row;

  @override
  bool operator ==(Object other) =>
      other is TerminalWheelCell && other.col == col && other.row == row;

  @override
  int get hashCode => Object.hash(col, row);
}

/// 业务逻辑：SGR wheel 报告按触点落格编码，贴近桌面滚轮落点语义，而不是恒发 1,1
/// （web MobileTerminalXtermSlot 触控换算：cellW=rect.width/max(cols,1)，col=clamp(floor(dx/cellW)+1)）。
///
/// Code Logic：[localDx]/[localDy] 为触点相对视口左上角的偏移；按视口与 cols/rows 均分字符格，
/// floor 后 +1 并 clamp 到 1..cells；视口尺寸非法、cols/rows 非正或触点越界时回退 1（web 失败
/// 回落 1,1）。结果可直接作为 [encodeSgrWheelReports] 的 col/row 参数。
TerminalWheelCell sgrWheelCellFromTouch({
  required double localDx,
  required double localDy,
  required double viewportWidth,
  required double viewportHeight,
  required int cols,
  required int rows,
}) {
  int axis(double local, double viewport, int cells) {
    if (!viewport.isFinite || viewport <= 0 || cells <= 0 || !local.isFinite) {
      return 1;
    }
    final cellSize = viewport / cells;
    if (!cellSize.isFinite || cellSize <= 0) {
      return 1;
    }
    final index = (local / cellSize).floor() + 1;
    if (index < 1) {
      return 1;
    }
    if (index > cells) {
      return cells;
    }
    return index;
  }

  return TerminalWheelCell(
    col: axis(localDx, viewportWidth, cols),
    row: axis(localDy, viewportHeight, rows),
  );
}

/// 业务逻辑：Claude Code 等 TUI 靠「鼠标滚轮」滚 transcript，拖动必须编码成与
/// xterm mouse tracking 相同的 SGR wheel 报告，而不是方向键（方向键是列表导航）。
///
/// Code Logic：SGR `CSI < Pb ; Px ; Py M`，wheel up Pb=64、wheel down Pb=65（xterm
/// CoreMouseService 权威值，xterm 4.0.0 包内 68/69 为非标准编号，不用）。
/// lines>0（上滑看新内容）→ 65；lines<0（看更旧历史）→ 64。col/row 为 1-based 字符格；
/// |lines| 截断到 [maxEvents]，返回值可直接经输入流发送；lines==0 返回空串。
String encodeSgrWheelReports(
  int lines, {
  int col = 1,
  int row = 1,
  int maxEvents = kTerminalTuiWheelEventsCap,
}) {
  if (lines == 0) {
    return '';
  }
  final cap = maxEvents > 0 ? maxEvents : kTerminalTuiWheelEventsCap;
  final count = lines.abs() > cap ? cap : lines.abs();
  if (count == 0) {
    return '';
  }
  final safeCol = col >= 1 ? col : 1;
  final safeRow = row >= 1 ? row : 1;
  final button = lines > 0 ? 65 : 64;
  return '\x1b[<$button;$safeCol;${safeRow}M' * count;
}

/// 业务逻辑：桌面滚轮/移动拖动的处置路径必须与 web 一致，避免把 TUI 拖成「列表导航」。
///
/// Code Logic：mouse tracking 已协商（any mode）→ 一律 SGR 转发；未协商但 alternate
/// screen（应用自管整屏）→ 同样 SGR 转发；未协商且 normal buffer：尚未用 owner 端
/// tmux 历史灌过 buffer 时先 hydration，之后才允许本地 scrollback。
TerminalScrollAction resolveTerminalScrollAction({
  required bool usingAltBuffer,
  required bool mouseTrackingNegotiated,
  required bool historyHydrated,
}) {
  if (mouseTrackingNegotiated || usingAltBuffer) {
    return TerminalScrollAction.forwardSgrWheel;
  }
  if (!historyHydrated) {
    return TerminalScrollAction.hydrateScrollback;
  }
  return TerminalScrollAction.localScrollback;
}

/// 业务逻辑：hydration 网络往返期间手指可能继续上滑，意图要累计且封顶，避免一次跳过整屏。
///
/// Code Logic：只累计向上（lines<0）的行数，clamp 到 [-rows, -1]；已有意图时取更靠上的值。
int accumulateHydrationScrollIntent(int current, int lines, int rows) {
  if (lines >= 0) {
    return current;
  }
  final minIntent = -(rows > 0 ? rows : 1);
  final merged = current + lines;
  if (merged > -1) {
    return -1;
  }
  return merged < minIntent ? minIntent : merged;
}
