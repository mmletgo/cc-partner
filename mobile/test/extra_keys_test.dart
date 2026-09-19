import 'package:cc_partner_mobile/terminal/extra_keys.dart';
import 'package:test/test.dart';

void main() {
  test('extra-key payloads cover Esc/Tab/Enter/Ctrl/Alt and arrows', () {
    final keys = getTerminalExtraKeys();
    final byId = {for (final key in keys) key.id: key};
    expect(byId['esc']!.payload, '\x1b');
    expect(byId['tab']!.payload, '\t');
    expect(byId['enter']!.payload, '\r');
    expect(byId['ctrl']!.kind, ExtraKeyKind.modifier);
    expect(byId['alt']!.kind, ExtraKeyKind.modifier);
    expect(byId['up']!.payload, '\x1b[A');
    expect(byId['down']!.payload, '\x1b[B');
    expect(byId['left']!.payload, '\x1b[D');
    expect(byId['right']!.payload, '\x1b[C');
    expect(byId['up']!.repeatable, isTrue);
    expect(byId['slash']!.repeatable, isNot(true));
  });

  test('/ long-press popup lists /clear /rewind /resume /compact', () {
    final slash = getTerminalExtraKeys().singleWhere((k) => k.id == 'slash');
    expect(extraKeyHasPopup(slash), isTrue);
    expect(
      slash.popup!.map((item) => item.payload).toList(),
      ['/clear', '/rewind', '/resume', '/compact'],
    );
    expect(kExtraKeyLongPressMs, 400);
    expect(kExtraKeyRepeatIntervalMs, 80);
  });

  test('sticky Ctrl/Alt transform the next character then consume', () {
    expect(applyStickyModifier(null, 'a').data, 'a');
    expect(applyStickyModifier(null, 'a').consume, isFalse);
    final ctrl = applyStickyModifier(StickyModifier.ctrl, 'c');
    expect(ctrl.data, '\x03');
    expect(ctrl.consume, isTrue);
    final alt = applyStickyModifier(StickyModifier.alt, 'x');
    expect(alt.data, '\x1bx');
    expect(toggleStickyModifier(null, StickyModifier.ctrl).armed, StickyModifier.ctrl);
    expect(toggleStickyModifier(StickyModifier.ctrl, StickyModifier.ctrl).armed, isNull);
  });

  test('sticky 武装 3 秒后应自动解除（可注入时钟），消耗/取消立即失效', () {
    expect(kStickyTimeoutMs, 3000);
    final hold = StickyModifierHold();
    final now = 1000000;
    expect(hold.isArmed, isFalse);
    expect(hold.shouldAutoDisarm(now), isFalse);
    hold.arm(now);
    expect(hold.isArmed, isTrue);
    expect(hold.shouldAutoDisarm(now + kStickyTimeoutMs - 1), isFalse);
    expect(hold.shouldAutoDisarm(now + kStickyTimeoutMs), isTrue);
    // 任意按键消耗后取消：即使超过 3 秒也不应再触发解除。
    hold.cancel();
    expect(hold.shouldAutoDisarm(now + kStickyTimeoutMs + 9999), isFalse);
    // 重新武装按新时刻计时。
    hold.arm(now + kStickyTimeoutMs);
    expect(hold.shouldAutoDisarm(now + 2 * kStickyTimeoutMs - 1), isFalse);
    expect(hold.shouldAutoDisarm(now + 2 * kStickyTimeoutMs), isTrue);
  });

  test('方向键连发参数：400ms 前置延迟 + 80ms 间隔', () {
    expect(kExtraKeyRepeatDelayMs, 400);
    expect(kExtraKeyRepeatIntervalMs, 80);
    final repeatable = getTerminalExtraKeys().where(extraKeyIsRepeatable).toList();
    expect(repeatable.map((key) => key.id), containsAll(['up', 'down', 'left', 'right']));
  });
}
