import 'package:flutter/material.dart';

import '../address_book/book.dart';
import '../core/lan_http.dart';
import '../git/client.dart';
import '../projects/client.dart';
import '../transfer/api.dart';

/// 把 ISO 时间格式化成本地 yyyy-MM-dd HH:mm；解析失败回退原始字符串。
String formatCommitTime(String value) {
  if (value.isEmpty) {
    return '—';
  }
  final time = DateTime.tryParse(value)?.toLocal();
  if (time == null) {
    return value;
  }
  String two(int n) => n.toString().padLeft(2, '0');
  return '${time.year.toString().padLeft(4, '0')}-${two(time.month)}-${two(time.day)} '
      '${two(time.hour)}:${two(time.minute)}';
}

class GitPage extends StatefulWidget {
  const GitPage({
    super.key,
    required this.book,
    required this.http,
    required this.project,
    this.worktreeId,
    this.gitClient,
  });

  final AddressBook book;
  final LanHttpClient http;
  final ProjectSummary project;
  final String? worktreeId;

  /// 测试可注入的 Git 客户端；缺省按当前 PC 地址簿构造。
  final GitClient? gitClient;

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
  List<WorkbenchGitCommit> _commits = [];
  bool _commitsLoading = false;
  String? _commitsError;

  @override
  void initState() {
    super.initState();
    _client = widget.gitClient ?? GitClient(widget.http, widget.book.active!.baseUrl);
    _reload();
  }

  /// 当前选中的 worktree：优先外部传入 id，其次主 worktree，最后第一个。
  Map<String, dynamic>? get _selectedTree {
    for (final tree in _trees) {
      if (tree['id'] == widget.worktreeId) {
        return tree;
      }
    }
    for (final tree in _trees) {
      if (tree['isMain'] == true) {
        return tree;
      }
    }
    return _trees.isEmpty ? null : _trees.first;
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

  /// 静默刷新 worktrees（含 Git 状态）+ 提交历史；下拉刷新与动作后复用。
  Future<void> _refresh() async {
    try {
      final body = await _client.listWorktrees(widget.project.id, includeGitStatus: true);
      final mapped = asObjectList(body, wrapKey: 'worktrees');
      if (mapped.isEmpty && body.containsKey('id')) {
        mapped.add(body);
      }
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
    await _loadCommits();
  }

  /// 加载当前选中 worktree 的最近 30 条提交；合并后源 worktree 可能已删除，此时清空历史。
  Future<void> _loadCommits() async {
    final tree = _selectedTree;
    final worktreeId = tree?['id'] as String?;
    if (tree == null || worktreeId == null || worktreeId.isEmpty) {
      if (mounted) {
        setState(() {
          _commits = [];
          _commitsError = null;
          _commitsLoading = false;
        });
      }
      return;
    }
    if (mounted) {
      setState(() {
        _commitsLoading = true;
        _commitsError = null;
      });
    }
    try {
      final commits = await _client.commits(widget.project.id, worktreeId: worktreeId);
      if (mounted) {
        setState(() {
          _commits = commits;
          _commitsLoading = false;
        });
      }
    } catch (error) {
      if (mounted) {
        setState(() {
          _commitsError = error.toString();
          _commitsLoading = false;
        });
      }
    }
  }

  /// 执行 Git 动作：busy 防重入；失败 SnackBar 上屏；hook 失败转修复卡；完成后刷新。
  Future<void> _run(String label, Future<Map<String, dynamic>> Function() action) async {
    if (_busy != null) {
      return;
    }
    setState(() => _busy = label);
    try {
      final result = await action();
      if (result['kind'] == 'failedHook' || result['hookFailure'] != null) {
        setState(() {
          _hookFailure = result['hookFailure'] as Map<String, dynamic>? ?? result;
          _hookWorktreeId = result['worktreeId'] as String?;
        });
      }
      await _refresh();
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

  /// pull/push/merge 前的确认框：说明对哪个 worktree 做什么。
  Future<void> _confirmThenRun(
    String label,
    Map<String, dynamic> tree,
    Future<Map<String, dynamic>> Function() action,
  ) async {
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: Text('确认$label'),
        content: Text('将对 worktree「${worktreeDisplayName(tree)}」执行$label。'),
        actions: [
          TextButton(onPressed: () => Navigator.pop(context, false), child: const Text('取消')),
          FilledButton(onPressed: () => Navigator.pop(context, true), child: const Text('确认')),
        ],
      ),
    );
    if (confirmed != true) {
      return;
    }
    await _run(label, action);
  }

  /// 提交前填写说明；说明框即确认步骤，说明对哪个 worktree 提交。
  Future<void> _commit(Map<String, dynamic> tree) async {
    final id = tree['id'] as String? ?? '';
    final controller = TextEditingController();
    final message = await showDialog<String>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('提交说明'),
        content: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text('将提交 worktree「${worktreeDisplayName(tree)}」的改动。'),
            const SizedBox(height: 8),
            TextField(
              controller: controller,
              autofocus: true,
              decoration: const InputDecoration(hintText: '提交信息'),
            ),
          ],
        ),
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

  /// 触发 hook AI 修复；成功清卡片并提示，失败 SnackBar 上屏。
  Future<void> _repairHook() async {
    final failure = _hookFailure;
    if (failure == null) {
      return;
    }
    try {
      await _client.repairHookFailure(
        worktreeId: _hookWorktreeId ?? widget.worktreeId ?? '',
        hookFailure: failure,
      );
      if (mounted) {
        setState(() => _hookFailure = null);
        ScaffoldMessenger.of(context).showSnackBar(
          const SnackBar(content: Text('已发起 hook AI 修复，请在终端查看进度。')),
        );
      }
    } catch (error) {
      if (mounted) {
        ScaffoldMessenger.of(context)
            .showSnackBar(SnackBar(content: Text('hook 修复失败: $error')));
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    final selected = _selectedTree;
    return RefreshIndicator(
      onRefresh: _refresh,
      child: ListView(
        physics: const AlwaysScrollableScrollPhysics(),
        padding: const EdgeInsets.fromLTRB(8, 8, 8, 24),
        children: [
          if (_busy != null) LinearProgressIndicator(key: ValueKey(_busy)),
          if (_loading)
            const Padding(
              padding: EdgeInsets.all(48),
              child: Center(child: CircularProgressIndicator()),
            )
          else ...[
            if (_error != null)
              Card(
                color: Theme.of(context).colorScheme.errorContainer,
                child: ListTile(
                  title: const Text('加载失败'),
                  subtitle: Text(_error!),
                  trailing: TextButton(onPressed: _reload, child: const Text('重试')),
                ),
              ),
            if (selected != null) _buildStatusCard(selected),
            if (_trees.isEmpty && _error == null)
              const Padding(
                padding: EdgeInsets.all(24),
                child: Center(child: Text('没有 worktree。')),
              ),
            if (_hookFailure != null)
              Card(
                color: Theme.of(context).colorScheme.errorContainer,
                child: ListTile(
                  title: const Text('Git hook 失败'),
                  subtitle: Text('${_hookFailure!['output'] ?? _hookFailure}'),
                  trailing: TextButton(onPressed: _repairHook, child: const Text('hook-repair')),
                ),
              ),
            for (final tree in _trees)
              Card(
                child: ListTile(
                  title: Text(worktreeDisplayName(tree)),
                  subtitle: Text(tree['branch'] as String? ?? ''),
                  selected: tree['id'] == widget.worktreeId,
                  trailing: PopupMenuButton<String>(
                    onSelected: (value) {
                      final id = tree['id'] as String? ?? '';
                      final op = newClientOperationId();
                      switch (value) {
                        case 'commit':
                          _commit(tree);
                        case 'pull':
                          _confirmThenRun('拉取', tree, () {
                            return _client.pull(projectId: widget.project.id, worktreeId: id);
                          });
                        case 'push':
                          _confirmThenRun('推送', tree, () {
                            return _client.push(projectId: widget.project.id, worktreeId: id);
                          });
                        case 'merge':
                          _confirmThenRun('合并', tree, () {
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
            _buildCommitsSection(selected),
          ],
        ],
      ),
    );
  }

  /// 状态卡：当前 worktree 的分支、工作区状态与 ahead/behind。
  Widget _buildStatusCard(Map<String, dynamic> tree) {
    final status = WorktreeGitStatus.of(tree);
    final branch = status.branch ?? tree['branch'] as String? ?? '—';
    String stateText;
    if (!status.present) {
      stateText = '状态未知';
    } else if (status.conflicts > 0) {
      stateText = '冲突（${status.conflicts} 个文件）';
    } else if (!status.clean || status.changed > 0) {
      stateText = '有改动（${status.changed} 个文件）';
    } else {
      stateText = '工作区干净';
    }
    return Card(
      child: Padding(
        padding: const EdgeInsets.all(12),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              children: [
                Expanded(
                  child: Text(
                    worktreeDisplayName(tree),
                    style: Theme.of(context).textTheme.titleMedium,
                  ),
                ),
                if (tree['isMain'] == true) const Chip(label: Text('主工作区')),
              ],
            ),
            const SizedBox(height: 8),
            _statusRow('分支', branch),
            _statusRow('状态', stateText),
            _statusRow('领先/落后', '领先 ${status.ahead} · 落后 ${status.behind}'),
          ],
        ),
      ),
    );
  }

  Widget _statusRow(String label, String value) {
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 2),
      child: Row(
        children: [
          SizedBox(
            width: 76,
            child: Text(label, style: Theme.of(context).textTheme.bodySmall),
          ),
          Expanded(child: Text(value)),
        ],
      ),
    );
  }

  /// 提交历史区：刷新按钮 + 加载/错误（重试）/空态 + 最近 30 条提交。
  Widget _buildCommitsSection(Map<String, dynamic>? selected) {
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Padding(
          padding: const EdgeInsets.fromLTRB(4, 12, 4, 4),
          child: Row(
            children: [
              Expanded(
                child: Text('最近提交', style: Theme.of(context).textTheme.titleMedium),
              ),
              TextButton.icon(
                onPressed: _busy == null ? _refresh : null,
                icon: const Icon(Icons.refresh),
                label: const Text('刷新'),
              ),
            ],
          ),
        ),
        if (selected == null)
          const Padding(
            padding: EdgeInsets.all(8),
            child: Text('选择 worktree 后查看提交历史。'),
          )
        else if (_commitsLoading)
          const Padding(
            padding: EdgeInsets.all(24),
            child: Center(child: CircularProgressIndicator()),
          )
        else if (_commitsError != null)
          Card(
            color: Theme.of(context).colorScheme.errorContainer,
            child: ListTile(
              title: const Text('提交历史加载失败'),
              subtitle: Text(_commitsError!),
              trailing: TextButton(onPressed: _loadCommits, child: const Text('重试')),
            ),
          )
        else if (_commits.isEmpty)
          const Padding(padding: EdgeInsets.all(8), child: Text('暂无提交。'))
        else
          for (final commit in _commits) _commitCard(commit),
      ],
    );
  }

  /// 单条提交：summary + shortHash + 作者/本地时间 + refs 徽章。
  Widget _commitCard(WorkbenchGitCommit commit) {
    return Card(
      child: Padding(
        padding: const EdgeInsets.all(12),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Expanded(
                  child: Text(
                    commit.summary.isEmpty ? commit.hash : commit.summary,
                    maxLines: 2,
                    overflow: TextOverflow.ellipsis,
                    style: Theme.of(context)
                        .textTheme
                        .bodyMedium
                        ?.copyWith(fontWeight: FontWeight.w600),
                  ),
                ),
                const SizedBox(width: 8),
                Text(
                  commit.shortHash.isEmpty ? '—' : commit.shortHash,
                  style: Theme.of(context)
                      .textTheme
                      .bodySmall
                      ?.copyWith(fontFamily: 'monospace'),
                ),
              ],
            ),
            const SizedBox(height: 4),
            Text(
              '${commit.authorName.isEmpty ? '未知作者' : commit.authorName}'
              ' · ${formatCommitTime(commit.authoredAt)}',
              style: Theme.of(context).textTheme.bodySmall,
            ),
            if (commit.refs.isNotEmpty)
              Padding(
                padding: const EdgeInsets.only(top: 6),
                child: Wrap(
                  spacing: 6,
                  runSpacing: 6,
                  children: [
                    for (final ref in commit.refs)
                      Chip(
                        label: Text(ref.name),
                        visualDensity: VisualDensity.compact,
                      ),
                  ],
                ),
              ),
          ],
        ),
      ),
    );
  }
}
