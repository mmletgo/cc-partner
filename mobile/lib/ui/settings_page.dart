import 'package:flutter/material.dart';

import '../address_book/book.dart';
import '../core/lan_http.dart';
import '../push/fanout.dart';
import '../settings/risk_copy.dart';
import 'provider_page.dart';

class SettingsPage extends StatelessWidget {
  const SettingsPage({super.key, required this.book, required this.http});

  final AddressBook book;
  final LanHttpClient http;

  @override
  Widget build(BuildContext context) {
    final server = book.active;
    final pushCap = server?.capabilities.contains(kMobilePushCapability) ?? false;
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
          title: const Text('系统推送'),
          subtitle: Text(
            pushCap
                ? '这台 PC 宣告 mobile.push.v1。未配置中转时不会发出通知。'
                : '这台 PC 不支持 mobile.push.v1。',
          ),
        ),
        ListTile(
          title: const Text('Provider'),
          subtitle: const Text('只切换当前这台电脑上已有的 provider'),
          onTap: () {
            Navigator.of(context).push(
              MaterialPageRoute(
                builder: (_) => ProviderPage(book: book, http: http),
              ),
            );
          },
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
