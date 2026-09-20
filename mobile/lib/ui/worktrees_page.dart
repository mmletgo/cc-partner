import 'package:flutter/material.dart';

import '../address_book/book.dart';
import '../core/lan_http.dart';
import '../git/client.dart';
import '../git/mutation.dart';
import '../git/project_sync.dart';
import '../projects/client.dart';
import '../sessions/client.dart';
import '../terminal/git_actions.dart';
import '../transfer/api.dart';
import 'worktree_strip.dart';

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
    this.confirmLeaveDirty,
    this.onWorktreesMutated,
  });

  final AddressBook book;
  final LanHttpClient http;
  final ProjectSummary project;
  final String? activeId;
  final ValueChanged<Map<String, dynamic>> onSelect;

  /// 测试可注入的 Git 客户端；缺省按当前 PC 地址簿构造。
  final GitClient? gitClient;

  /// 测试可注入的终端窗口创建器；缺省用 SessionsClient.create（自动开绑定窗口）。
  final Future<SessionSummary> Function(String projectId, String worktreeId)?
  onCreateSession;

  /// 删除激活 worktree 前的脏文件预检（由壳层注入 _confirmLeaveDirty，与 GitPage
  /// 同款固定接缝契约——页面拿不到 FileWorkspaceController）；取消时终止删除。
  final Future<bool> Function(String worktreeId)? confirmLeaveDirty;

  /// 删除/合并成功（含 unknown 对账确认成功）后通知壳层统一收敛：重拉权威列表
  /// （active 失效按主树优先回落，strip 不残留已删树）+ bump 终端会话刷新令牌
  /// （对齐 web MobileWorktreePanel 的 onWorktreesChange/onRefreshSessions 回写；
  /// 固定接缝契约）。
  final VoidCallback? onWorktreesMutated;

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

  /// mutation 相位机：删除/合并 unknown 后锁定动作并要求同 id 对账（对齐 web bar controller）。
  final GitMutationTracker _tracker = GitMutationTracker();

  /// mutation 结果未知等错误文案（条内横幅展示，unknown 相位带「重新对账」）。
  String? _mutationError;

  @override
  void initState() {
    super.initState();
    _client =
        widget.gitClient ?? GitClient(widget.http, widget.book.active!.baseUrl);
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
      final body = await _client.listWorktrees(
        widget.project.id,
        includeGitStatus: true,
      );
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

  void _showSnack(String message, {bool transient = false}) {
    ScaffoldMessenger.of(context).showSnackBar(
      SnackBar(
        content: Text(message),
        duration: transient
            ? const Duration(milliseconds: 2500)
            : const Duration(seconds: 4),
      ),
    );
  }

  /// 动作按钮统一禁用条件：常规 busy 或 mutation 相位未回 idle。
  bool get _actionLocked => _busy || _tracker.actionLocked;

  /// Business Logic: unknown 后必须用同一 clientOperationId 查 ledger + 权威列表对账
  /// （共享 reconcileWorktreeMutation 通道）；成功走成功流程、失败提示、仍 unknown 保持横幅。
  /// Code Logic: 相位推进入 reconciling → 对账 → settleReconcile 落终态或保持 unknown。
  Future<void> _reconcile(
    String operationId, {
    String? successMessage,
    Future<void> Function()? onSuccess,
  }) async {
    _tracker.beginReconcile();
    if (mounted) {
      setState(() => _busy = true);
    }
    try {
      final result = await reconcileWorktreeMutation(
        client: _client,
        projectId: widget.project.id,
        operationId: operationId,
      );
      _tracker.settleReconcile(result);
      if (!mounted) {
        return;
      }
      if (result == GitMutationReconcile.confirmedSucceeded) {
        setState(() => _mutationError = null);
        if (onSuccess != null) {
          await onSuccess();
        } else {
          await _refresh();
        }
        if (successMessage != null) {
          _showSnack(successMessage, transient: true);
        }
      } else if (result == GitMutationReconcile.confirmedFailed) {
        setState(() => _mutationError = null);
        _showSnack('操作失败，可以重新发起。');
      } else {
        setState(() => _mutationError = '操作结果未知，请重新对账。');
      }
    } finally {
      if (mounted) {
        setState(() => _busy = false);
      }
    }
  }

  /// unknown 相位横幅上的「重新对账」：复用同一 operationId，不盲重放。
  Future<void> _retryReconcile() async {
    final operationId = _tracker.operationId;
    if (operationId == null ||
        _tracker.phase != GitMutationPhase.unknown ||
        _busy) {
      return;
    }
    await _reconcile(operationId);
  }

  /// 创建 worktree 并自动开绑定终端窗口（对齐 web createWorktreeWithTerminalWindow）。
  ///
  /// Business Logic: 用户新建 worktree 后下一步就是进终端，所以创建成功要自动开窗口并切过去；
  /// 窗口创建失败时保留 worktree、只报错不回滚（对齐 web 行为）。
  /// Code Logic: 共享 createWorktreeWithTerminalSession（create → sessions/create，失败不回滚）→
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
    final result = await createWorktreeWithTerminalSession(
      git: _client,
      sessions: SessionsClient(widget.http, widget.book.active!.baseUrl),
      projectId: widget.project.id,
      branchName: branch,
      onCreateSession: widget.onCreateSession,
    );
    if (result.createError != null) {
      if (mounted) {
        setState(() => _busy = false);
        _showSnack('创建失败: ${result.createError}');
      }
      return;
    }
    await _refresh();
    if (!mounted) {
      return;
    }
    setState(() => _busy = false);
    _suffix.clear();
    final created = result.created;
    if (result.sessionError != null) {
      _showSnack('终端窗口创建失败（worktree 已保留）: ${result.sessionError}');
    } else {
      _showSnack('已创建 worktree「$branch」');
    }
    final newId = created?['id'] as String?;
    if (created != null && newId != null && newId.isNotEmpty) {
      // 最后再通知 shell：切换 worktree 并自动进入终端面板。
      widget.onSelect(created);
    }
  }

  /// Business Logic: 删除 worktree 在 unknown（envelope 或传输异常）时禁止盲重放，
  /// 必须用同一 clientOperationId 对账（对齐 web pickMobileMutationOperationId + 对账矩阵）。
  /// Code Logic: 确认框 → tracker.begin(remove) 锁定 → remove envelope：
  ///   succeeded → 刷新+提示；unknown → 共享对账通道；传输异常 → unknown 横幅；
  ///   服务器应答的确定失败 → 解锁 + SnackBar。
  Future<void> _remove(Map<String, dynamic> tree) async {
    if (_actionLocked) {
      return;
    }
    final id = tree['id'] as String? ?? '';
    if (id.isEmpty) {
      return;
    }
    final name = worktreeDisplayName(tree);
    // 对齐 web runMobileWorktreeRemovalFlow：删除激活 worktree 前先做只读脏文件预检，
    // 取消则不调后端；选择丢弃会由壳层清 dirty 快照，未保存草稿不再随删除静默丢失。
    if (id == widget.activeId &&
        widget.confirmLeaveDirty != null &&
        !await widget.confirmLeaveDirty!(id)) {
      return;
    }
    if (!mounted) {
      return;
    }
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('移除 worktree'),
        // 对齐 web MobileWorktreePanel removeConfirm（worktrees 列表卡口径，
        // 与切换条的「关闭终端窗口」重口径区分）。
        content: Text('确定移除 worktree“$name”？请先确认不再需要该工作区。'),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context, false),
            child: const Text('取消'),
          ),
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
    final operationId = _tracker.begin(
      kind: GitMutationKind.remove,
      worktreeId: id,
      nextOperationId: newClientOperationId(),
    );
    setState(() {
      _busy = true;
      _mutationError = null;
    });
    try {
      final envelope = GitMutationEnvelope.from(
        await _client.remove(worktreeId: id, clientOperationId: operationId),
      );
      if (envelope.succeeded) {
        _tracker.markIdle();
        await _afterRemoveSuccess();
        if (mounted) {
          _showSnack('已移除 worktree「$name」');
        }
        return;
      }
      if (envelope.unknown) {
        await _reconcile(
          envelope.clientOperationId ?? operationId,
          successMessage: '已移除 worktree「$name」',
          onSuccess: _afterRemoveSuccess,
        );
        return;
      }
      _tracker.markIdle();
      if (mounted) {
        _showSnack('移除失败: 后端返回了未知的结果形态');
      }
    } catch (error) {
      if (isTransportUnknownError(error)) {
        // 请求可能已到达也可能没到达：进入 unknown 横幅等对账，禁止盲重试。
        _tracker.markUnknown();
        if (mounted) {
          setState(() => _mutationError = '移除结果未知，请重新对账。');
        }
      } else {
        _tracker.markIdle();
        if (mounted) {
          _showSnack('移除失败: $error');
        }
      }
    } finally {
      if (mounted) {
        setState(() => _busy = false);
      }
    }
  }

  /// Business Logic: worktrees 页卡片要能直接发起合并（对齐 web MobileWorktreePanel 卡片动作）；
  /// merge 会删除源 worktree 或收集分支，unknown 时同样走同 id 对账，禁止新 id 盲重放。
  /// Code Logic: 共享文案确认框 → 源树是激活树时先做只读 dirty 预检（对齐 web
  /// runMobileWorktreeMergeFlow：后端会先删源树，预检必须在前端先行，取消则不调后端）→
  /// tracker.begin(merge) → merge envelope →
  ///   succeeded → 合并成功 + 刷新 + 源树是 active 时 onSelect(主树) 交 shell 兜底切换；
  ///   unknown → 共享对账；传输异常 → unknown 横幅；确定失败 → 解锁 + SnackBar。
  Future<void> _merge(Map<String, dynamic> tree) async {
    if (_actionLocked) {
      return;
    }
    final id = tree['id'] as String? ?? '';
    if (id.isEmpty || tree['isMain'] == true) {
      return;
    }
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('合并到主工作区'),
        content: Text(worktreeMergeConfirmText(tree)),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context, false),
            child: const Text('取消'),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(context, true),
            child: const Text('合并'),
          ),
        ],
      ),
    );
    if (confirmed != true) {
      return;
    }
    // 只读 dirty 预检与删除同款接缝（confirmLeaveDirty 由壳层注入）；取消时不调后端。
    if (id == widget.activeId &&
        widget.confirmLeaveDirty != null &&
        !await widget.confirmLeaveDirty!(id)) {
      return;
    }
    if (!mounted) {
      return;
    }
    final operationId = _tracker.begin(
      kind: GitMutationKind.merge,
      worktreeId: id,
      nextOperationId: newClientOperationId(),
    );
    setState(() {
      _busy = true;
      _mutationError = null;
    });
    try {
      final envelope = GitMutationEnvelope.from(
        await _client.merge(
          projectId: widget.project.id,
          worktreeId: id,
          clientOperationId: operationId,
        ),
      );
      if (envelope.succeeded) {
        _tracker.markIdle();
        await _afterMergeSuccess(tree);
        return;
      }
      if (envelope.unknown) {
        await _reconcile(
          envelope.clientOperationId ?? operationId,
          successMessage: '合并成功',
          onSuccess: () => _afterMergeSuccess(tree),
        );
        return;
      }
      _tracker.markIdle();
      if (mounted) {
        _showSnack('合并失败: 后端返回了未知的结果形态');
      }
    } catch (error) {
      if (isTransportUnknownError(error)) {
        _tracker.markUnknown();
        if (mounted) {
          setState(() => _mutationError = '合并结果未知，请重新对账。');
        }
      } else {
        _tracker.markIdle();
        if (mounted) {
          _showSnack('合并失败: $error');
        }
      }
    } finally {
      if (mounted) {
        setState(() => _busy = false);
      }
    }
  }

  /// 删除成功共享出口：页内刷新 + 通知壳层统一收敛（权威列表回落 active、strip
  /// 同步移除、终端会话缓冲清理）。
  Future<void> _afterRemoveSuccess() async {
    await _refresh();
    widget.onWorktreesMutated?.call();
  }

  /// Business Logic: merge 成功后源 worktree 可能已被删除；源树正是当前 active 时
  /// 必须把选择交给 shell 兜底（resolveActiveWorktreeId 回落主树，dirty guard 保留）。
  /// Code Logic: 刷新列表 → 通知壳层统一收敛 → SnackBar「合并成功」→ merged 树是
  /// activeId 时回调 onSelect(主树/首项)。
  Future<void> _afterMergeSuccess(Map<String, dynamic> tree) async {
    await _refresh();
    widget.onWorktreesMutated?.call();
    if (!mounted) {
      return;
    }
    _showSnack('合并成功', transient: true);
    final mergedId = tree['id'] as String? ?? '';
    if (mergedId.isEmpty || mergedId != widget.activeId) {
      return;
    }
    final next =
        pickMainWorktree(_trees) ?? (_trees.isEmpty ? null : _trees.first);
    final nextId = next?['id'] as String?;
    if (next != null && nextId != null && nextId.isNotEmpty) {
      widget.onSelect(next);
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
          if (_busy) const LinearProgressIndicator(),
          if (_tracker.phase == GitMutationPhase.reconciling)
            const Padding(padding: EdgeInsets.all(8), child: Text('核对结果中…')),
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
                trailing: TextButton(
                  onPressed: _reload,
                  child: const Text('重试'),
                ),
              ),
            if (_tracker.phase == GitMutationPhase.unknown &&
                _mutationError != null)
              Card(
                key: const Key('worktrees-unknown-banner'),
                color: theme.colorScheme.errorContainer,
                child: ListTile(
                  title: const Text('操作结果未知'),
                  subtitle: Text(_mutationError!),
                  trailing: TextButton(
                    key: const Key('worktrees-retry-reconcile'),
                    onPressed: _busy ? null : _retryReconcile,
                    child: const Text('重新对账'),
                  ),
                ),
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
                    onPressed: _busy || _suffix.text.trim().isEmpty
                        ? null
                        : _create,
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

  /// 单个 worktree 卡片：主/linked 标记 + 分支名 + 路径 + 状态/同步/可推送徽章；
  /// 非主卡动作区带「合并」（B14，unknown 对账共享相位机）与「移除」。
  Widget _treeCard(ThemeData theme, Map<String, dynamic> tree) {
    final id = tree['id'] as String? ?? '';
    final isMain = tree['isMain'] == true;
    final status = WorktreeGitStatus.of(tree);
    return Card(
      child: ListTile(
        key: Key('worktree-item-$id'),
        selected: id == widget.activeId,
        // 对齐 web MobileWorktreePanel：删除/合并/创建/对账在途（页内忙碌锁）时
        // 卡片选择禁用（onTap 置 null 同时呈现禁用视觉），锁定期间不触发 onSelect。
        onTap: _actionLocked ? null : () => widget.onSelect(tree),
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
            _badge(theme, isMain ? '主工作区' : 'worktree', emphasized: isMain),
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
                style: theme.textTheme.bodySmall?.copyWith(
                  color: theme.colorScheme.onSurfaceVariant,
                ),
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
            : Row(
                mainAxisSize: MainAxisSize.min,
                children: [
                  IconButton(
                    key: Key('worktree-merge-$id'),
                    icon: const Icon(Icons.merge_type),
                    tooltip: '合并',
                    onPressed: _actionLocked ? null : () => _merge(tree),
                  ),
                  IconButton(
                    key: Key('worktree-delete-$id'),
                    icon: const Icon(Icons.delete_outline),
                    tooltip: '移除',
                    onPressed: _actionLocked ? null : () => _remove(tree),
                  ),
                ],
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
