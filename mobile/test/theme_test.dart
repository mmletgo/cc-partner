import 'package:cc_partner_mobile/settings/theme.dart';
import 'package:flutter/material.dart';
import 'package:test/test.dart';

void main() {
  test('theme preference persists light and dark', () {
    final store = MemoryThemeStore();
    expect(store.load(), ThemeMode.dark);
    store.save(ThemeMode.light);
    expect(store.load(), ThemeMode.light);
    store.save(ThemeMode.dark);
    expect(store.load(), ThemeMode.dark);
  });
}
