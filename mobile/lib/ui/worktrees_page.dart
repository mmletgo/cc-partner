import 'package:flutter/material.dart';

import '../address_book/book.dart';
import '../core/lan_http.dart';
import '../git/client.dart';
import '../projects/client.dart';
import '../transfer/api.dart';

class WorktreesPage extends StatefulWidget {
  const WorktreesPage({
    super.key,
    required this.book,
    required this.http,
    required this.project,
    required this.activeId,
    required this.onSelect,
  });

  final AddressBook book;
  final LanHttpClient http;
  final ProjectSummary project;
  final String? activeId;
  final ValueChanged<Map<String, dynamic>> onSelect;

  @override
  State<WorktreesPage> createState() => _WorktreesPageState();
}

class _WorktreesPageState extends State<WorktreesPage> {
  late final GitClient _client;
  List<Map<String, dynamic>> _trees = [];
  String? _error;
  bool _loading = true;
  final _branch = TextEditingController();

  @override
  void initState() {
    super.initState();
    _client = GitClient(widget.http, widget.book.active!.baseUrl);
    _reload();
  }

  @override
  void dispose() {
    _branch.dispose();
    super.dispose();
  }

  Future<void> _reload() async {
    setState(() {
      _loading = true;
      _error = null;
    });
    try {
      final body = await _client.listWorktrees(widget.project.id);
      final mapped = asObjectList(body, wrapKey: 'worktrees');
      if (mounted) {
        setState(() {
          _trees = mapped;
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

  @override
  Widget build(BuildContext context) {
    if (_loading) {
      return const Center(child: CircularProgressIndicator());
    }
    return ListView(
      children: [
        if (_error != null) ListTile(title: Text(_error!)),
        Padding(
          padding: const EdgeInsets.all(8),
          child: Row(
            children: [
              Expanded(
                child: TextField(
                  controller: _branch,
                  decoration: const InputDecoration(hintText: '新分支名'),
                ),
              ),
              FilledButton(
                onPressed: () async {
                  await _client.create(
                    projectId: widget.project.id,
                    branchName: _branch.text.trim(),
                  );
                  _branch.clear();
                  await _reload();
                },
                child: const Text('创建'),
              ),
            ],
          ),
        ),
        for (final tree in _trees)
          ListTile(
            selected: tree['id'] == widget.activeId,
            title: Text(tree['name'] as String? ?? tree['branch'] as String? ?? ''),
            subtitle: Text(tree['branch'] as String? ?? tree['id'] as String? ?? ''),
            onTap: () => widget.onSelect(tree),
            trailing: tree['isMain'] == true
                ? null
                : IconButton(
                    icon: const Icon(Icons.delete_outline),
                    onPressed: () async {
                      await _client.remove(
                        worktreeId: tree['id'] as String? ?? '',
                        clientOperationId: newClientOperationId(),
                      );
                      await _reload();
                    },
                  ),
          ),
      ],
    );
  }
}
