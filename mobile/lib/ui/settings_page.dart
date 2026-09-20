import 'package:flutter/material.dart';

import '../address_book/book.dart';
import '../app.dart';
import '../core/lan_http.dart';
import '../push/fanout.dart';
import '../settings/risk_copy.dart';
import '../settings/theme.dart';

class SettingsPage extends StatelessWidget {
  const SettingsPage({super.key, required this.book, required this.http});

  final AddressBook book;
  final LanHttpClient http;

  @override
  Widget build(BuildContext context) {
    final server = book.active;
    final pushCap = server?.capabilities.contains(kMobilePushCapability) ?? false;
    final theme = AppThemeScope.of(context);
    return ListView(
      padding: const EdgeInsets.all(16),
      children: [
        Card(
          color: Theme.of(context).colorScheme.errorContainer,
          child: Padding(
            padding: const EdgeInsets.all(12),
            child: Text(kLanRiskStatement),
          ),
        ),
        const SizedBox(height: 12),
        ListTile(
          title: const Text('当前电脑'),
          subtitle: Text(server?.baseUrl ?? '未选择'),
        ),
        ListTile(
          title: const Text('外观'),
          subtitle: Text(themeModeDisplayName(theme.mode)),
          trailing: SegmentedButton<ThemeMode>(
            key: const Key('theme-mode-segmented'),
            segments: const [
              ButtonSegment(value: ThemeMode.system, label: Text('系统')),
              ButtonSegment(value: ThemeMode.light, label: Text('浅色')),
              ButtonSegment(value: ThemeMode.dark, label: Text('深色')),
            ],
            selected: {theme.mode},
            onSelectionChanged: (selection) {
              if (selection.isNotEmpty) {
                theme.onChanged(selection.first);
              }
            },
          ),
        ),
        ListTile(
          title: const Text('系统推送'),
          subtitle: Text(
            pushCap
                ? '这台 PC 宣告 mobile.push.v1。未配置中转时不会发出通知。'
                : '这台 PC 不支持 mobile.push.v1。',
          ),
        ),
        ListTile(
          title: const Text('地址簿'),
          subtitle: const Text('扫码或手填，切换电脑'),
          onTap: () => Navigator.of(context).popUntil((route) => route.isFirst),
        ),
      ],
    );
  }
}
