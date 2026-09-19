import 'package:flutter/material.dart';

import '../address_book/book.dart';
import '../core/lan_http.dart';
import '../git/client.dart';
import '../projects/client.dart';
import '../transfer/api.dart';

class GitPage extends StatefulWidget {
  const GitPage({super.key, required this.book, required this.http, required this.project});

  final AddressBook book;
  final LanHttpClient http;
  final ProjectSummary project;

  @override
  State<GitPage> createState() => _GitPageState();
}

class _GitPageState extends State<GitPage> {
  late final GitClient _client;
  List<Map<String, dynamic>> _trees = [];
  String? _error;
  bool _loading = true;
  String? _busy;

  @override
  void initState() {
    super.initState();
    _client = GitClient(widget.http, widget.book.active!.baseUrl);
    _reload();
  }

  Future<void> _reload() async {
    setState(() {
      _loading = true;
      _error = null;
    });
    try {
      final body = await _client.listWorktrees(widget.project.id);
      final mapped = asObjectList(body, wrapKey: 'worktrees');
      if (mapped.isEmpty && body.containsKey('id')) {
        mapped.add(body);
      }
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

  Future<void> _run(String label, Future<void> Function() action) async {
    setState(() => _busy = label);
    try {
      await action();
      await _reload();
    } catch (error) {
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text('$label 失败: $error')));
      }
    } finally {
      if (mounted) {
        setState(() => _busy = null);
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    if (_loading) {
      return const Center(child: CircularProgressIndicator());
    }
    if (_error != null) {
      return Center(child: Text(_error!));
    }
    if (_trees.isEmpty) {
      return const Center(child: Text('没有 worktree。'));
    }
    return ListView(
      children: [
        if (_busy != null) LinearProgressIndicator(key: ValueKey(_busy)),
        for (final tree in _trees)
          Card(
            child: ListTile(
              title: Text(tree['name'] as String? ?? tree['branch'] as String? ?? tree['id'] as String? ?? ''),
              subtitle: Text(tree['branch'] as String? ?? ''),
              trailing: PopupMenuButton<String>(
                onSelected: (value) {
                  final id = tree['id'] as String? ?? '';
                  final op = newClientOperationId();
                  switch (value) {
                    case 'commit':
                      _run('提交', () async {
                        await _client.commit(worktreeId: id, clientOperationId: op);
                      });
                    case 'pull':
                      _run('拉取', () async {
                        await _client.pull(projectId: widget.project.id, worktreeId: id);
                      });
                    case 'push':
                      _run('推送', () async {
                        await _client.push(projectId: widget.project.id, worktreeId: id);
                      });
                    case 'merge':
                      _run('合并', () async {
                        await _client.merge(
                          projectId: widget.project.id,
                          worktreeId: id,
                          clientOperationId: op,
                        );
                      });
                  }
                },
                itemBuilder: (_) => const [
                  PopupMenuItem(value: 'commit', child: Text('提交')),
                  PopupMenuItem(value: 'pull', child: Text('拉取')),
                  PopupMenuItem(value: 'push', child: Text('推送')),
                  PopupMenuItem(value: 'merge', child: Text('合并')),
                ],
              ),
            ),
          ),
      ],
    );
  }
}
