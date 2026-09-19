import 'package:cc_partner_mobile/terminal/touch_scroll.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  group('clampTerminalDimension', () {
    test('非法输入回退下限，四舍五入后 clamp 到 min..65535', () {
      expect(clampTerminalDimension(double.nan, kMinTerminalCols), kMinTerminalCols);
      expect(clampTerminalDimension(double.infinity, kMinTerminalRows), kMinTerminalRows);
      expect(clampTerminalDimension(0, kMinTerminalCols), kMinTerminalCols);
      expect(clampTerminalDimension(5, kMinTerminalRows), kMinTerminalRows);
      expect(clampTerminalDimension(19.4, kMinTerminalCols), kMinTerminalCols);
      expect(clampTerminalDimension(80.6, kMinTerminalCols), 81);
      expect(clampTerminalDimension(70000, kMinTerminalCols), kMaxTerminalDimension);
      expect(clampTerminalDimension(120, kMinTerminalCols), 120);
    });
  });

  group('terminalTouchLineHeight', () {
    test('优先 viewport/rows，异常回退 fallback，至少 1px', () {
      expect(terminalTouchLineHeight(600, 24, 13), 25.0);
      expect(terminalTouchLineHeight(0, 24, 13), 13.0);
      expect(terminalTouchLineHeight(600, 0, 13), 13.0);
      expect(terminalTouchLineHeight(double.nan, 24, 0), 1.0);
      expect(terminalTouchLineHeight(600, 2000, 13), 1.0);
    });
  });

  group('updateTouchScroll', () {
    test('像素位移换算整数行并保留余量', () {
      var state = const TouchScrollState(lastClientY: 100);
      // 手指上滑 55px、行高 25 → 2 行（向底部，lines>0）。
      var result = updateTouchScroll(state, 45, 25);
      expect(result.lines, 2);
      expect(result.state.remainderPx, closeTo(5, 1e-9));
      // 继续 3px → 余量 8px 不足一行。
      result = updateTouchScroll(result.state, 42, 25);
      expect(result.lines, 0);
      expect(result.state.remainderPx, closeTo(8, 1e-9));
      // 下滑 100px → 余量 8-100=-92 → -3 行（向顶部），新余量 -92+75=-17。
      result = updateTouchScroll(result.state, 142, 25);
      expect(result.lines, -3);
      expect(result.state.lastClientY, 142);
      expect(result.state.remainderPx, closeTo(-17, 1e-9));
    });

    test('非法行高按 1px 处理', () {
      final state = const TouchScrollState(lastClientY: 10);
      final result = updateTouchScroll(state, 4, -1);
      expect(result.lines, 6);
    });
  });

  group('encodeSgrWheelReports', () {
    test('上滑编码 wheel down(65)，下滑编码 wheel up(64)', () {
      expect(encodeSgrWheelReports(2), '\x1b[<65;1;1M\x1b[<65;1;1M');
      expect(encodeSgrWheelReports(-2), '\x1b[<64;1;1M\x1b[<64;1;1M');
      expect(encodeSgrWheelReports(1, col: 3, row: 5), '\x1b[<65;3;5M');
      expect(encodeSgrWheelReports(-1, col: 0, row: -2), '\x1b[<64;1;1M');
    });

    test('单次最多 8 事件，0 行返回空串', () {
      expect(encodeSgrWheelReports(100), '\x1b[<65;1;1M' * 8);
      expect(encodeSgrWheelReports(-100), '\x1b[<64;1;1M' * 8);
      expect(encodeSgrWheelReports(100, maxEvents: 3), '\x1b[<65;1;1M' * 3);
      expect(encodeSgrWheelReports(0), '');
      expect(encodeSgrWheelReports(0, maxEvents: -5), '');
    });
  });

  group('resolveTerminalScrollAction', () {
    test('mouse tracking 已协商 → 一律 SGR 转发', () {
      expect(
        resolveTerminalScrollAction(
          usingAltBuffer: false,
          mouseTrackingNegotiated: true,
          historyHydrated: false,
        ),
        TerminalScrollAction.forwardSgrWheel,
      );
      expect(
        resolveTerminalScrollAction(
          usingAltBuffer: false,
          mouseTrackingNegotiated: true,
          historyHydrated: true,
        ),
        TerminalScrollAction.forwardSgrWheel,
      );
    });

    test('alternate screen → SGR 转发（不信任本地 baseY）', () {
      expect(
        resolveTerminalScrollAction(
          usingAltBuffer: true,
          mouseTrackingNegotiated: false,
          historyHydrated: true,
        ),
        TerminalScrollAction.forwardSgrWheel,
      );
    });

    test('normal buffer 未协商：未 hydration 先灌历史，之后本地 scrollback', () {
      expect(
        resolveTerminalScrollAction(
          usingAltBuffer: false,
          mouseTrackingNegotiated: false,
          historyHydrated: false,
        ),
        TerminalScrollAction.hydrateScrollback,
      );
      expect(
        resolveTerminalScrollAction(
          usingAltBuffer: false,
          mouseTrackingNegotiated: false,
          historyHydrated: true,
        ),
        TerminalScrollAction.localScrollback,
      );
    });
  });

  group('accumulateHydrationScrollIntent', () {
    test('只累计向上意图并 clamp 到 [-rows, -1]', () {
      expect(accumulateHydrationScrollIntent(0, 3, 24), 0);
      expect(accumulateHydrationScrollIntent(0, -1, 24), -1);
      expect(accumulateHydrationScrollIntent(-3, -5, 24), -8);
      expect(accumulateHydrationScrollIntent(-20, -20, 24), -24);
      expect(accumulateHydrationScrollIntent(-24, -5, 24), -24);
      expect(accumulateHydrationScrollIntent(0, -1, 0), -1);
    });
  });
}
