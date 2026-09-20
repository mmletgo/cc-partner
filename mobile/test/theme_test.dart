import 'dart:io';

import 'package:cc_partner_mobile/address_book/book.dart';
import 'package:cc_partner_mobile/app.dart';
import 'package:cc_partner_mobile/core/lan_http.dart';
import 'package:cc_partner_mobile/settings/theme.dart';
import 'package:cc_partner_mobile/ui/settings_page.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  test('theme preference persists light and dark', () {
    final store = MemoryThemeStore();
    store.save(ThemeMode.light);
    expect(store.load(), ThemeMode.light);
    store.save(ThemeMode.dark);
    expect(store.load(), ThemeMode.dark);
  });

  test('memory theme store defaults to system (align web prefers-color-scheme)', () {
    final store = MemoryThemeStore();
    expect(store.load(), ThemeMode.system);
    store.save(ThemeMode.system);
    expect(store.load(), ThemeMode.system);
  });

  test('file theme store round-trips all three modes', () {
    final dir = Directory.systemTemp.createTempSync('cc-partner-theme-test');
    addTearDown(() => dir.deleteSync(recursive: true));
    final store = FileThemeStore(File('${dir.path}/theme.txt'));
    for (final mode in ThemeMode.values) {
      store.save(mode);
      expect(store.load(), mode, reason: '${mode.name} 应原样落盘并读回');
    }
  });

  test('file theme store falls back to system for legacy-less or broken files', () {
    final dir = Directory.systemTemp.createTempSync('cc-partner-theme-test');
    addTearDown(() => dir.deleteSync(recursive: true));
    // 文件缺失：默认跟随系统（旧版本默认 dark，这里对齐 web 无存储值回落系统偏好）。
    expect(
      FileThemeStore(File('${dir.path}/missing.txt')).load(),
      ThemeMode.system,
    );
    // 文件损坏/空内容：宽容回落 system，不 crash。
    final broken = File('${dir.path}/broken.txt')..writeAsStringSync('not-a-mode');
    expect(FileThemeStore(broken).load(), ThemeMode.system);
    // 旧版本遗留的 dark/light 值保持原语义不迁移。
    final legacy = File('${dir.path}/legacy.txt')..writeAsStringSync('dark');
    expect(FileThemeStore(legacy).load(), ThemeMode.dark);
    legacy.writeAsStringSync('light');
    expect(FileThemeStore(legacy).load(), ThemeMode.light);
  });

  test('theme mode display name covers all three modes', () {
    expect(themeModeDisplayName(ThemeMode.system), '跟随系统');
    expect(themeModeDisplayName(ThemeMode.light), '浅色');
    expect(themeModeDisplayName(ThemeMode.dark), '深色');
  });

  testWidgets('settings appearance row is a three-state selector synced to scope', (
    tester,
  ) async {
    ThemeMode current = ThemeMode.system;
    final book = AddressBook(store: MemoryAddressBookStore());
    late StateSetter setScope;
    await tester.pumpWidget(
      MaterialApp(
        home: StatefulBuilder(
          builder: (context, setState) {
            setScope = setState;
            return AppThemeScope(
              mode: current,
              onChanged: (mode) => setState(() => current = mode),
              // SettingsPage 实际运行在壳层 Scaffold 内，这里提供等价 Material 宿主。
              child: Scaffold(body: SettingsPage(book: book, http: LanHttpClient())),
            );
          },
        ),
      ),
    );

    // 默认跟随系统：副标题随动。
    expect(find.text('外观'), findsOneWidget);
    expect(find.text('跟随系统'), findsOneWidget);
    final segmented = tester.widget<SegmentedButton<ThemeMode>>(
      find.byKey(const Key('theme-mode-segmented')),
    );
    expect(segmented.selected, {ThemeMode.system});

    // 切到深色：回调写入 scope，副标题随动。
    await tester.tap(find.text('深色'));
    await tester.pumpAndSettle();
    expect(current, ThemeMode.dark);
    expect(find.text('深色'), findsNWidgets(2), reason: '段标签 + 副标题都显示深色');

    // 切到浅色 / 跟随系统同样生效。
    await tester.tap(find.text('浅色'));
    await tester.pumpAndSettle();
    expect(current, ThemeMode.light);
    setScope(() => current = ThemeMode.system);
    await tester.pumpAndSettle();
    expect(find.text('跟随系统'), findsOneWidget);
  });
}
