import 'dart:io';

import 'package:flutter/material.dart';

abstract class ThemeStore {
  ThemeMode load();
  void save(ThemeMode mode);
}

class MemoryThemeStore implements ThemeStore {
  ThemeMode _mode = ThemeMode.dark;

  @override
  ThemeMode load() => _mode;

  @override
  void save(ThemeMode mode) {
    _mode = mode;
  }
}

class FileThemeStore implements ThemeStore {
  FileThemeStore(this.file);

  final File file;

  @override
  ThemeMode load() {
    try {
      if (!file.existsSync()) {
        return ThemeMode.dark;
      }
      final raw = file.readAsStringSync().trim();
      return raw == 'light' ? ThemeMode.light : ThemeMode.dark;
    } catch (_) {
      return ThemeMode.dark;
    }
  }

  @override
  void save(ThemeMode mode) {
    file.parent.createSync(recursive: true);
    file.writeAsStringSync(mode == ThemeMode.light ? 'light' : 'dark');
  }
}
