import 'package:flutter/material.dart';

import '../address_book/book.dart';
import '../address_book/models.dart';
import '../core/lan_http.dart';
import 'attention_page.dart';
import 'projects_page.dart';
import 'settings_page.dart';
import 'transfer_page.dart';

/// Dual-mode workbench: 项目 / 待处理 / 传输 / 设置.
class WorkbenchHome extends StatefulWidget {
  const WorkbenchHome({super.key, required this.book, required this.http});

  final AddressBook book;
  final LanHttpClient http;

  @override
  State<WorkbenchHome> createState() => _WorkbenchHomeState();
}

class _WorkbenchHomeState extends State<WorkbenchHome> {
  int _index = 0;

  ServerRecord get _server => widget.book.active!;

  @override
  Widget build(BuildContext context) {
    final pages = [
      ProjectsPage(book: widget.book, http: widget.http),
      AttentionPage(book: widget.book, http: widget.http),
      TransferPage(book: widget.book, http: widget.http),
      SettingsPage(book: widget.book, http: widget.http),
    ];
    final label = _server.name.isNotEmpty
        ? _server.name
        : (_server.deviceName ?? _server.baseUrl);
    return Scaffold(
      appBar: AppBar(
        title: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(label, overflow: TextOverflow.ellipsis),
            Text(_server.baseUrl, style: Theme.of(context).textTheme.bodySmall),
          ],
        ),
      ),
      body: pages[_index],
      bottomNavigationBar: NavigationBar(
        selectedIndex: _index,
        onDestinationSelected: (value) => setState(() => _index = value),
        destinations: const [
          NavigationDestination(icon: Icon(Icons.folder_outlined), label: '项目'),
          NavigationDestination(icon: Icon(Icons.inbox_outlined), label: '待处理'),
          NavigationDestination(icon: Icon(Icons.swap_horiz), label: '传输'),
          NavigationDestination(icon: Icon(Icons.settings_outlined), label: '设置'),
        ],
      ),
    );
  }
}
