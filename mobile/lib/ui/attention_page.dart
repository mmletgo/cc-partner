import 'package:flutter/material.dart';

import '../address_book/book.dart';
import '../attention/client.dart';
import '../attention/filter.dart';
import '../core/lan_http.dart';
import '../projects/client.dart';

class AttentionPage extends StatefulWidget {
  const AttentionPage({
    super.key,
    required this.book,
    required this.http,
    required this.onNavigate,
  });

  final AddressBook book;
  final LanHttpClient http;
  final void Function(ProjectSummary project, String panel, String? sessionId) onNavigate;

  @override
  State<AttentionPage> createState() => _AttentionPageState();
}

class _AttentionPageState extends State<AttentionPage> {
  List<AttentionItem> _items = [];
  String? _error;
  bool _loading = true;

  @override
  void initState() {
    super.initState();
    _reload();
  }

  Future<void> _reload() async {
    setState(() {
      _loading = true;
      _error = null;
    });
    try {
      final items = await AttentionClient(widget.http, widget.book.active!.baseUrl).listVisible();
      if (mounted) {
        setState(() {
          _items = items;
          _loading = false;
        });
      }
    } catch (error) {
      if (mounted) {
        setState(() {
          _error = error.toString();
          _loading = false;
        });
      }
    }
  }

  Future<void> _open(AttentionItem item) async {
    final nav = navigateAttention(item);
    if (nav.projectId == null) {
      return;
    }
    final projects = await ProjectsClient(widget.http, widget.book.active!.baseUrl).listRecent();
    final project = projects.where((p) => p.id == nav.projectId).firstOrNull;
    if (!mounted || project == null) {
      return;
    }
    widget.onNavigate(project, nav.panel, nav.sessionId);
  }

  @override
  Widget build(BuildContext context) {
    if (_loading) {
      return const Center(child: CircularProgressIndicator());
    }
    if (_error != null) {
      return Center(child: Text(_error!));
    }
    if (_items.isEmpty) {
      return const Center(child: Text('没有待处理事项。'));
    }
    return RefreshIndicator(
      onRefresh: _reload,
      child: ListView(
        children: _items
            .map(
              (item) => ListTile(
                title: Text(item.title ?? item.id),
                subtitle: Text(item.sourceKind),
                onTap: () => _open(item),
              ),
            )
            .toList(),
      ),
    );
  }
}
