import 'dart:io';

import 'package:flutter/material.dart';

abstract class ThemeStore {
  ThemeMode load();
  void save(ThemeMode mode);
}

/// Business Logic: 主题三态选择要给用户可读文案（对齐 web useTheme 的
/// system/light/dark 语义与 workbench.theme 文案：跟随系统/浅色/深色）。
/// Code Logic: 纯函数映射；未知值理论上不存在（ThemeMode 枚举穷举）。
String themeModeDisplayName(ThemeMode mode) {
  switch (mode) {
    case ThemeMode.system:
      return '跟随系统';
    case ThemeMode.light:
      return '浅色';
    case ThemeMode.dark:
      return '深色';
  }
}

class MemoryThemeStore implements ThemeStore {
  /// 默认跟随系统（对齐 web useTheme 无存储值时回落 prefers-color-scheme）。
  ThemeMode _mode = ThemeMode.system;

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

  /// Business Logic: 主题持久化值需要宽容解析：旧版本只写过 dark/light，
  /// 新增 'system'（跟随系统）为默认；文件缺失或损坏时也回落 system，
  /// 与 web「无存储值回落 prefers-color-scheme」口径一致。
  /// Code Logic: 读文件 trim 后精确匹配 light/dark；其余（含缺失/异常）返回 system。
  @override
  ThemeMode load() {
    try {
      if (!file.existsSync()) {
        return ThemeMode.system;
      }
      final raw = file.readAsStringSync().trim();
      switch (raw) {
        case 'light':
          return ThemeMode.light;
        case 'dark':
          return ThemeMode.dark;
        default:
          return ThemeMode.system;
      }
    } catch (_) {
      return ThemeMode.system;
    }
  }

  /// Business Logic: 三态都要能落盘，且旧值 dark/light 保持原字符串不迁移，
  /// 只有显式选择「跟随系统」才写 'system'。
  /// Code Logic: 按 ThemeMode 映射字符串后写入；父目录不存在时先递归创建。
  @override
  void save(ThemeMode mode) {
    file.parent.createSync(recursive: true);
    final raw = switch (mode) {
      ThemeMode.light => 'light',
      ThemeMode.dark => 'dark',
      ThemeMode.system => 'system',
    };
    file.writeAsStringSync(raw);
  }
}
