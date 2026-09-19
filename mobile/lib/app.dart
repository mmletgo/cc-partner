import 'package:flutter/material.dart';

import 'address_book/book.dart';
import 'core/lan_http.dart';
import 'settings/theme.dart';
import 'ui/address_book_page.dart';

class AppThemeScope extends InheritedWidget {
  const AppThemeScope({
    super.key,
    required this.mode,
    required this.onChanged,
    required super.child,
  });

  final ThemeMode mode;
  final ValueChanged<ThemeMode> onChanged;

  static AppThemeScope of(BuildContext context) {
    final scope = context.dependOnInheritedWidgetOfExactType<AppThemeScope>();
    if (scope == null) {
      throw StateError('AppThemeScope missing');
    }
    return scope;
  }

  @override
  bool updateShouldNotify(AppThemeScope oldWidget) => mode != oldWidget.mode;
}

/// Root Flutter app. Workbench is self-drawn, not a WebView of `/mobile`.
class CcPartnerApp extends StatefulWidget {
  const CcPartnerApp({
    super.key,
    required this.book,
    required this.http,
    this.probe,
    this.themeStore,
  });

  final AddressBook book;
  final LanHttpClient http;
  final HealthProbe? probe;
  final ThemeStore? themeStore;

  @override
  State<CcPartnerApp> createState() => _CcPartnerAppState();
}

class _CcPartnerAppState extends State<CcPartnerApp> {
  late ThemeStore _store;
  late ThemeMode _mode;

  @override
  void initState() {
    super.initState();
    _store = widget.themeStore ?? MemoryThemeStore();
    _mode = _store.load();
  }

  @override
  Widget build(BuildContext context) {
    const seed = Color(0xFFC96442);
    return AppThemeScope(
      mode: _mode,
      onChanged: (mode) {
        _store.save(mode);
        setState(() => _mode = mode);
      },
      child: MaterialApp(
        title: 'cc-partner',
        theme: ThemeData(
          colorScheme: ColorScheme.fromSeed(seedColor: seed),
          useMaterial3: true,
        ),
        darkTheme: ThemeData(
          colorScheme: ColorScheme.fromSeed(seedColor: seed, brightness: Brightness.dark),
          useMaterial3: true,
        ),
        themeMode: _mode,
        home: AddressBookPage(book: widget.book, http: widget.http, probe: widget.probe),
      ),
    );
  }
}
