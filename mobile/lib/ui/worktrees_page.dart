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
    this.gitClient,
  });

  final AddressBook book;
  final LanHttpClient http;
  final ProjectSummary project;
  final String? activeId;
  final ValueChanged<Map<String, dynamic>> onSelect;

  /// 测试可注入的 Git 客户端；缺省按当前 PC 地址簿构造。
  final GitClient? gitClient;

  @override
  State<WorktreesPage> createState() => _WorktreesPageState();
}

class _WorktreesPageState extends State<WorktreesPage> {
  late final GitClient _client;
  List<Map<String, dynamic>> _trees = [];
  String? _error;
  bool _loading = true;
  bool _busy = false;
  final _branch = TextEditingController();

  @override
  void initState() {
    super.initState();
    _client = widget.gitClient ?? GitClient(widget.http, widget.book.active!.baseUrl);
    _reload();
  }

  @override
  void dispose() {
    _branch.dispose();
    super.dispose();
  }

  /// 首次进入全量刷新：整页加载态。
  Future<void> _reload() async {
    setState(() {
      _loading = true;
      _error = null;
    });
    await _refresh();
    if (mounted) {
      setState(() => _loading = false);
    }
  }

  /// 静默刷新列表；下拉刷新与创建/删除后复用。
  Future<void> _refresh() async {
    try {
      final body = await _client.listWorktrees(widget.project.id);
      final mapped = asObjectList(body, wrapKey: 'worktrees');
      if (mounted) {
        setState(() {
          _trees = mapped;
          _error = null;
        });
      }
    } catch (error) {
      if (mounted) {
        setState(() => _error = error.toString());
      }
    }
  }

  void _showSnack(String message) {
    ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(message)));
  }

  /// 创建 worktree：空名校验、busy 防重复提交；失败 SnackBar 上屏，成功刷新列表并提示。
  Future<void> _create() async {
    if (_busy) {
      return;
    }
    final branch = _branch.text.trim();
    if (branch.isEmpty) {
      _showSnack('请先输入分支名');
      return;
    }
    setState(() => _busy = true);
    try {
      await _client.create(projectId: widget.project.id, branchName: branch);
      _branch.clear();
      await _refresh();
      if (mounted) {
        _showSnack('已创建 worktree「$branch」');
      }
    } catch (error) {
      if (mounted) {
        _showSnack('创建失败: $error');
      }
    } finally {
      if (mounted) {
        setState(() => _busy = false);
      }
    }
  }

  /// 删除 worktree：先弹确认框说明目标与风险；失败 SnackBar 上屏，成功刷新列表并提示。
  Future<void> _remove(Map<String, dynamic> tree) async {
    if (_busy) {
      return;
    }
    final id = tree['id'] as String? ?? '';
    if (id.isEmpty) {
      return;
    }
    final name = worktreeDisplayName(tree);
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('删除 worktree'),
        content: Text('确定删除 worktree「$name」吗？未推送的提交可能丢失。'),
        actions: [
          TextButton(onPressed: () => Navigator.pop(context, false), child: const Text('取消')),
          FilledButton(
            style: FilledButton.styleFrom(
              backgroundColor: Theme.of(context).colorScheme.error,
            ),
            onPressed: () => Navigator.pop(context, true),
            child: const Text('删除'),
          ),
        ],
      ),
    );
    if (confirmed != true) {
      return;
    }
    setState(() => _busy = true);
    try {
      await _client.remove(
        worktreeId: id,
        clientOperationId: newClientOperationId(),
      );
      await _refresh();
      if (mounted) {
        _showSnack('已删除 worktree「$name」');
      }
    } catch (error) {
      if (mounted) {
        _showSnack('删除失败: $error');
      }
    } finally {
      if (mounted) {
        setState(() => _busy = false);
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    return RefreshIndicator(
      onRefresh: _refresh,
      child: ListView(
        physics: const AlwaysScrollableScrollPhysics(),
        padding: const EdgeInsets.only(bottom: 24),
        children: [
          if (_loading)
            const Padding(
              padding: EdgeInsets.all(48),
              child: Center(child: CircularProgressIndicator()),
            )
          else ...[
            if (_error != null)
              ListTile(
                leading: const Icon(Icons.error_outline),
                title: Text(_error!),
                trailing: TextButton(onPressed: _reload, child: const Text('重试')),
              ),
            Padding(
              padding: const EdgeInsets.all(8),
              child: Row(
                children: [
                  Expanded(
                    child: TextField(
                      controller: _branch,
                      enabled: !_busy,
                      decoration: const InputDecoration(hintText: '新分支名'),
                    ),
                  ),
                  FilledButton(
                    onPressed: _busy ? null : _create,
                    child: _busy
                        ? const SizedBox(
                            width: 16,
                            height: 16,
                            child: CircularProgressIndicator(strokeWidth: 2),
                          )
                        : const Text('创建'),
                  ),
                ],
              ),
            ),
            for (final tree in _trees)
              ListTile(
                key: Key('worktree-item-${tree['id']}'),
                selected: tree['id'] == widget.activeId,
                title: Text(worktreeDisplayName(tree)),
                subtitle: Text(tree['branch'] as String? ?? tree['id'] as String? ?? ''),
                onTap: () => widget.onSelect(tree),
                trailing: tree['isMain'] == true
                    ? null
                    : IconButton(
                        icon: const Icon(Icons.delete_outline),
                        tooltip: '删除',
                        onPressed: _busy ? null : () => _remove(tree),
                      ),
              ),
          ],
        ],
      ),
    );
  }
}
