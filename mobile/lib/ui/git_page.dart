import 'package:flutter/material.dart';

import '../address_book/book.dart';
import '../core/lan_http.dart';
import '../git/client.dart';
import '../projects/client.dart';
import '../transfer/api.dart';

class GitPage extends StatefulWidget {
  const GitPage({
    super.key,
    required this.book,
    required this.http,
    required this.project,
    this.worktreeId,
  });

  final AddressBook book;
  final LanHttpClient http;
  final ProjectSummary project;
  final String? worktreeId;

  @override
  State<GitPage> createState() => _GitPageState();
}

class _GitPageState extends State<GitPage> {
  late final GitClient _client;
  List<Map<String, dynamic>> _trees = [];
  String? _error;
  bool _loading = true;
  String? _busy;
  Map<String, dynamic>? _hookFailure;
  String? _hookWorktreeId;

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

  Future<void> _run(String label, Future<Map<String, dynamic>> Function() action) async {
    setState(() => _busy = label);
    try {
      final result = await action();
      if (result['kind'] == 'failedHook' || result['hookFailure'] != null) {
        setState(() {
          _hookFailure = result['hookFailure'] as Map<String, dynamic>? ?? result;
          _hookWorktreeId = result['worktreeId'] as String?;
        });
      }
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

  Future<void> _commit(String id) async {
    final controller = TextEditingController();
    final message = await showDialog<String>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('提交说明'),
        content: TextField(controller: controller, autofocus: true),
        actions: [
          TextButton(onPressed: () => Navigator.pop(context), child: const Text('取消')),
          FilledButton(
            onPressed: () => Navigator.pop(context, controller.text.trim()),
            child: const Text('提交'),
          ),
        ],
      ),
    );
    if (message == null || message.isEmpty) {
      return;
    }
    await _run('提交', () {
      return _client.commit(
        worktreeId: id,
        clientOperationId: newClientOperationId(),
        message: message,
      );
    });
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
        if (_hookFailure != null)
          Card(
            color: Theme.of(context).colorScheme.errorContainer,
            child: ListTile(
              title: const Text('Git hook 失败'),
              subtitle: Text('${_hookFailure!['output'] ?? _hookFailure}'),
              trailing: TextButton(
                onPressed: () async {
                  await _client.repairHookFailure(
                    worktreeId: _hookWorktreeId ?? widget.worktreeId ?? '',
                    hookFailure: _hookFailure!,
                  );
                  if (mounted) {
                    setState(() => _hookFailure = null);
                  }
                },
                child: const Text('hook-repair'),
              ),
            ),
          ),
        for (final tree in _trees)
          Card(
            child: ListTile(
              title: Text(tree['name'] as String? ?? tree['branch'] as String? ?? tree['id'] as String? ?? ''),
              subtitle: Text(tree['branch'] as String? ?? ''),
              selected: tree['id'] == widget.worktreeId,
              trailing: PopupMenuButton<String>(
                onSelected: (value) {
                  final id = tree['id'] as String? ?? '';
                  final op = newClientOperationId();
                  switch (value) {
                    case 'commit':
                      _commit(id);
                    case 'pull':
                      _run('拉取', () {
                        return _client.pull(projectId: widget.project.id, worktreeId: id);
                      });
                    case 'push':
                      _run('推送', () {
                        return _client.push(projectId: widget.project.id, worktreeId: id);
                      });
                    case 'merge':
                      _run('合并', () {
                        return _client.merge(
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
