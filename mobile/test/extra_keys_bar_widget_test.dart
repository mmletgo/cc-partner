import 'package:cc_partner_mobile/ui/extra_keys_bar.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

Future<void> _pumpBar(WidgetTester tester, List<String> sent, {bool disabled = false}) async {
  await tester.pumpWidget(
    MaterialApp(
      home: Scaffold(
        body: ExtraKeysBar(
          onSend: sent.add,
          sticky: null,
          onSticky: (_) {},
          disabled: disabled,
        ),
      ),
    ),
  );
  await tester.pump();
}

void main() {
  testWidgets('disabled：整条置灰，按键/长按连发/粘滞均不生效', (tester) async {
    final sent = <String>[];
    await _pumpBar(tester, sent, disabled: true);

    // 每键 onPressed 均为 null（对齐 web disabled 透传每键置灰）。
    for (final button in tester.widgetList<OutlinedButton>(find.byType(OutlinedButton))) {
      expect(button.onPressed, isNull);
    }

    // 普通键不发送。
    await tester.tap(find.widgetWithText(OutlinedButton, '/'));
    await tester.pump();
    expect(sent, isEmpty);

    // 方向键长按不连发。
    final upKey = find.widgetWithText(OutlinedButton, '↑');
    final gesture = await tester.startGesture(tester.getCenter(upKey));
    await tester.pump(const Duration(milliseconds: 500));
    await tester.pump(const Duration(milliseconds: 600));
    expect(sent, isEmpty);
    await gesture.up();
    await tester.pump();

    // 恢复可用后正常发送。
    await _pumpBar(tester, sent);
    await tester.tap(find.widgetWithText(OutlinedButton, '/'));
    await tester.pump();
    expect(sent, ['/']);
  });

  testWidgets('方向键长按：立即发 1 次，前置延迟后按 80ms 连发，松手停止', (tester) async {
    final sent = <String>[];
    await _pumpBar(tester, sent);
    final upKey = find.widgetWithText(OutlinedButton, '↑');
    expect(upKey, findsOneWidget);

    final gesture = await tester.startGesture(tester.getCenter(upKey));
    // 触发长按识别（500ms），此时应立即发送第一发。
    await tester.pump(const Duration(milliseconds: 500));
    expect(sent, ['\x1b[A']);

    // 前置延迟 400ms 内不连发。
    await tester.pump(const Duration(milliseconds: 398));
    expect(sent.length, 1);

    // 越过前置延迟后恰好多发一次（连发周期尚未 tick）。
    await tester.pump(const Duration(milliseconds: 102));
    expect(sent.length, 2);

    // 之后按 80ms 稳定连发：410ms 窗口恰含 5 次周期触发。
    await tester.pump(const Duration(milliseconds: 410));
    expect(sent.length, 7);

    // 松手后停止连发。
    await gesture.up();
    await tester.pump(const Duration(milliseconds: 200));
    expect(sent.length, 7);
  });

  testWidgets('/ 短按插入斜杠', (tester) async {
    final sent = <String>[];
    await _pumpBar(tester, sent);
    await tester.tap(find.widgetWithText(OutlinedButton, '/'));
    await tester.pump();
    expect(sent, ['/']);
  });

  testWidgets('/ 长按按按键实际位置弹出命令菜单并可选择命令', (tester) async {
    final sent = <String>[];
    await _pumpBar(tester, sent);
    await tester.longPress(find.widgetWithText(OutlinedButton, '/'));
    await tester.pumpAndSettle();
    expect(find.text('/clear'), findsOneWidget);
    expect(find.text('/rewind'), findsOneWidget);

    await tester.tap(find.text('/clear'));
    await tester.pumpAndSettle();
    expect(sent, ['/clear']);
  });
}
