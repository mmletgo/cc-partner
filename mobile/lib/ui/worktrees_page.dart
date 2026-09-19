import 'package:flutter/material.dart';

import '../address_book/book.dart';
import '../core/lan_http.dart';
import '../git/client.dart';
import '../projects/client.dart';
import '../sessions/client.dart';
import '../transfer/api.dart';

/// Business Logic: worktrees 页与切换条共用「干净/有改动/冲突」三态文案（对齐 web status）。
/// Code Logic: conflicts 优先，其次 changed，最后干净；status 缺失时按干净展示（宽容解析）。
String worktreeStatusLabel(Map<String, dynamic> tree) {
  final status = WorktreeGitStatus.of(tree);
  if (status.conflicts > 0) {
    return '${status.conflicts} 处冲突';
  }
  if (!status.clean || status.changed > 0) {
    return '${status.changed} 处改动';
  }
  return '干净';
}

/// Business Logic: 卡片要展示与远端的同步差距（对齐 web ahead/behind badge）。
/// Code Logic: 拼接「领先 N / 落后 N」。
String worktreeSyncLabel(Map<String, dynamic> tree) {
  final status = WorktreeGitStatus.of(tree);
  return '领先 ${status.ahead} / 落后 ${status.behind}';
}

/// Business Logic: 用户要能区分哪些 worktree 还推不上去（对齐 web canPush badge）。
/// Code Logic: status.canPush 为 true → 「可推送」，否则「不可推送」。
String worktreeCanPushLabel(Map<String, dynamic> tree) =>
    WorktreeGitStatus.of(tree).canPush ? '可推送' : '不可推送';

class WorktreesPage extends StatefulWidget {
  const WorktreesPage({
    super.key,
    required this.book,
    required this.http,
    required this.project,
    required this.activeId,
    required this.onSelect,
    this.gitClient,
    this.onCreateSession,
  });

  final AddressBook book;
  final LanHttpClient http;
  final ProjectSummary project;
  final String? activeId;
  final ValueChanged<Map<String, dynamic>> onSelect;

  /// 测试可注入的 Git 客户端；缺省按当前 PC 地址簿构造。
  final GitClient? gitClient;

  /// 测试可注入的终端窗口创建器；缺省用 SessionsClient.create（自动开绑定窗口）。
  final Future<SessionSummary> Function(String projectId, String worktreeId)? onCreateSession;

  @override
  State<WorktreesPage> createState() => _WorktreesPageState();
}

class _WorktreesPageState extends State<WorktreesPage> {
  late final GitClient _client;
  List<Map<String, dynamic>> _trees = [];
  String? _error;
  bool _loading = true;
  bool _busy = false;
  String _prefix = kDefaultWorktreeBranchPrefix;
  final _suffix = TextEditingController();

  @override
  void initState() {
    super.initState();
    _client = widget.gitClient ?? GitClient(widget.http, widget.book.active!.baseUrl);
    _reload();
  }

  @override
  void dispose() {
    _suffix.dispose();
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

  /// 静默刷新列表（含 Git 状态）；下拉刷新与创建/删除后复用。
  Future<void> _refresh() async {
    try {
      final body = await _client.listWorktrees(widget.project.id, includeGitStatus: true);
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

  /// 创建 worktree 并自动开绑定终端窗口（对齐 web createWorktreeWithTerminalWindow）。
  ///
  /// Business Logic: 用户新建 worktree 后下一步就是进终端，所以创建成功要自动开窗口并切过去；
  /// 窗口创建失败时保留 worktree、只报错不回滚（对齐 web 行为）。
  /// Code Logic: compose 前缀/后缀 → worktrees/create → sessions/create（失败不回滚）→
  /// 刷新列表 → 提示 → onSelect 通知 shell 切 worktree 并进终端面板。
  Future<void> _create() async {
    if (_busy) {
      return;
    }
    final branch = composeWorktreeBranchName(_prefix, _suffix.text);
    if (branch == null) {
      _showSnack('请先输入分支后缀');
      return;
    }
    setState(() => _busy = true);
    Map<String, dynamic>? created;
    try {
      created = await _client.create(projectId: widget.project.id, branchName: branch);
    } catch (error) {
      if (mounted) {
        _showSnack('创建失败: $error');
      }
    }
    if (created == null) {
      if (mounted) {
        setState(() => _busy = false);
      }
      return;
    }
    final newId = created['id'] as String?;
    Object? sessionError;
    if (newId != null && newId.isNotEmpty) {
      try {
        final opener = widget.onCreateSession;
        if (opener != null) {
          await opener(widget.project.id, newId);
        } else {
          await SessionsClient(widget.http, widget.book.active!.baseUrl)
              .create(widget.project.id, worktreeId: newId);
        }
      } catch (error) {
        sessionError = error;
      }
    }
    await _refresh();
    if (!mounted) {
      return;
    }
    setState(() => _busy = false);
    _suffix.clear();
    if (sessionError != null) {
      _showSnack('终端窗口创建失败（worktree 已保留）: $sessionError');
    } else {
      _showSnack('已创建 worktree「$branch」');
    }
    if (newId != null && newId.isNotEmpty) {
      // 最后再通知 shell：切换 worktree 并自动进入终端面板。
      widget.onSelect(Map<String, dynamic>.from(created));
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
        title: const Text('移除 worktree'),
        content: Text('确定移除 worktree「$name」吗？未推送的提交可能丢失。'),
        actions: [
          TextButton(onPressed: () => Navigator.pop(context, false), child: const Text('取消')),
          FilledButton(
            style: FilledButton.styleFrom(
              backgroundColor: Theme.of(context).colorScheme.error,
            ),
            onPressed: () => Navigator.pop(context, true),
            child: const Text('移除'),
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
        _showSnack('已移除 worktree「$name」');
      }
    } catch (error) {
      if (mounted) {
        _showSnack('移除失败: $error');
      }
    } finally {
      if (mounted) {
        setState(() => _busy = false);
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
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
                  DropdownButton<String>(
                    key: const Key('worktree-prefix-select'),
                    value: _prefix,
                    items: [
                      for (final prefix in kWorktreeBranchPrefixes)
                        DropdownMenuItem(value: prefix, child: Text(prefix)),
                    ],
                    onChanged: _busy
                        ? null
                        : (value) {
                            if (value != null) {
                              setState(() => _prefix = value);
                            }
                          },
                  ),
                  const Padding(
                    padding: EdgeInsets.symmetric(horizontal: 4),
                    child: Text('/'),
                  ),
                  Expanded(
                    child: TextField(
                      key: const Key('worktree-suffix-input'),
                      controller: _suffix,
                      enabled: !_busy,
                      onChanged: (_) => setState(() {}),
                      decoration: const InputDecoration(hintText: 'my-task'),
                    ),
                  ),
                  const SizedBox(width: 8),
                  FilledButton(
                    onPressed: _busy || _suffix.text.trim().isEmpty ? null : _create,
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
            if (_trees.isEmpty && _error == null)
              const Padding(
                padding: EdgeInsets.all(24),
                child: Center(child: Text('暂无 worktree')),
              ),
            for (final tree in _trees) _treeCard(theme, tree),
          ],
        ],
      ),
    );
  }

  /// 单个 worktree 卡片：主/linked 标记 + 分支名 + 路径 + 状态/同步/可推送徽章。
  Widget _treeCard(ThemeData theme, Map<String, dynamic> tree) {
    final id = tree['id'] as String? ?? '';
    final isMain = tree['isMain'] == true;
    final status = WorktreeGitStatus.of(tree);
    return Card(
      child: ListTile(
        key: Key('worktree-item-$id'),
        selected: id == widget.activeId,
        onTap: () => widget.onSelect(tree),
        title: Row(
          children: [
            Container(
              width: 8,
              height: 8,
              margin: const EdgeInsets.only(right: 6),
              decoration: BoxDecoration(
                shape: BoxShape.circle,
                color: status.conflicts > 0
                    ? theme.colorScheme.error
                    : (!status.clean || status.changed > 0)
                        ? theme.colorScheme.tertiary
                        : theme.colorScheme.primary,
              ),
            ),
            Expanded(
              child: Text(
                worktreeDisplayName(tree),
                overflow: TextOverflow.ellipsis,
              ),
            ),
            _badge(
              theme,
              isMain ? '主工作区' : 'worktree',
              emphasized: isMain,
            ),
          ],
        ),
        subtitle: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(status.branch ?? tree['branch'] as String? ?? '—'),
            if ((tree['path'] as String?)?.isNotEmpty == true)
              Text(
                tree['path'] as String,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: theme.textTheme.bodySmall
                    ?.copyWith(color: theme.colorScheme.onSurfaceVariant),
              ),
            const SizedBox(height: 4),
            Wrap(
              spacing: 6,
              runSpacing: 4,
              children: [
                _badge(theme, worktreeStatusLabel(tree)),
                _badge(theme, worktreeSyncLabel(tree)),
                _badge(
                  theme,
                  worktreeCanPushLabel(tree),
                  emphasized: status.canPush,
                ),
              ],
            ),
          ],
        ),
        trailing: isMain
            ? null
            : IconButton(
                key: Key('worktree-delete-$id'),
                icon: const Icon(Icons.delete_outline),
                tooltip: '移除',
                onPressed: _busy ? null : () => _remove(tree),
              ),
      ),
    );
  }

  /// 小徽章胶囊：emphasized 用主色容器，否则用中性容器。
  Widget _badge(ThemeData theme, String text, {bool emphasized = false}) {
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 2),
      decoration: BoxDecoration(
        color: emphasized
            ? theme.colorScheme.primaryContainer
            : theme.colorScheme.surfaceContainerHighest,
        borderRadius: BorderRadius.circular(999),
      ),
      child: Text(
        text,
        style: theme.textTheme.labelSmall?.copyWith(
          color: emphasized
              ? theme.colorScheme.onPrimaryContainer
              : theme.colorScheme.onSurfaceVariant,
        ),
      ),
    );
  }
}
