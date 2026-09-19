import 'dart:async';

import 'package:flutter/material.dart';

import '../address_book/book.dart';
import '../core/lan_http.dart';
import '../git/client.dart';
import '../git/mutation.dart';
import '../git/project_sync.dart';
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

/// Git 动作成功的自动消失时长（对齐 web useAutoDismissedStatus 的 2.5s）。
const Duration kGitSuccessSnackbarDuration = Duration(milliseconds: 2500);

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

  /// 全量项目 DTO（含 deviceId/gitRemoteFingerprint），用于「同步」找其他设备兄弟项目。
  List<Map<String, dynamic>> _allProjects = [];
  String? _error;
  bool _loading = true;

  /// 当前正在执行的动作标签；非空时展示进度条并禁用动作。
  String? _busy;

  /// mutation 相位机：busy/reconciling/unknown 期间锁定全部动作并要求对账。
  final GitMutationTracker _tracker = GitMutationTracker();

  Map<String, dynamic>? _hookFailure;
  String? _hookWorktreeId;
  List<WorkbenchGitCommit> _commits = [];
  bool _commitsLoading = false;
  String? _commitsError;

  /// 用户在 Git 页内点选的 worktree（优先于 shell 传入的 worktreeId）。
  String? _localSelectedId;

  @override
  void initState() {
    super.initState();
    _client = widget.gitClient ?? GitClient(widget.http, widget.book.active!.baseUrl);
    _reload();
  }

  /// 当前选中的 worktree：页内点选 > 外部传入 id > 主 worktree > 第一个。
  Map<String, dynamic>? get _selectedTree {
    final preferredId = _localSelectedId ?? widget.worktreeId;
    for (final tree in _trees) {
      if (tree['id'] == preferredId) {
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

  /// 动作按钮统一禁用条件：有动作在跑或 mutation 相位未回到 idle。
  bool get _actionDisabled => _busy != null || _tracker.actionLocked;

  /// 首次进入全量刷新：整页加载态 + 兄弟项目清单。
  Future<void> _reload() async {
    setState(() {
      _loading = true;
      _error = null;
    });
    await _refresh();
    await _loadProjects();
    if (mounted) {
      setState(() => _loading = false);
    }
  }

  /// Business Logic: 「同步」按钮的可用性依赖其他设备兄弟项目数量，
  /// 需要含 deviceId/gitRemoteFingerprint 的全量项目清单；失败按空表处理（按钮禁用，fail-closed）。
  /// Code Logic: listAllProjects 宽容解析；异常时静默保留空列表。
  Future<void> _loadProjects() async {
    try {
      final projects = await _client.listAllProjects();
      if (mounted) {
        setState(() => _allProjects = projects);
      }
    } catch (_) {}
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

  /// 只刷新 worktrees（不动提交历史）；unknown 对账时复用。
  Future<void> _refreshWorktreesOnly() async {
    try {
      final body = await _client.listWorktrees(widget.project.id, includeGitStatus: true);
      final mapped = asObjectList(body, wrapKey: 'worktrees');
      if (mounted) {
        setState(() {
          _trees = mapped;
          _error = null;
        });
      }
    } catch (_) {}
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

  void _showSnack(String message, {bool transient = false}) {
    ScaffoldMessenger.of(context).showSnackBar(
      SnackBar(
        content: Text(message),
        duration: transient ? kGitSuccessSnackbarDuration : const Duration(seconds: 4),
      ),
    );
  }

  /// Business Logic: 提交/推送/合并/拉取返回 unknown（网络异常等无法确定结果）时禁止盲重放，
  /// 必须用同一 clientOperationId 查 ledger + 权威列表对账出「实际已成功/未生效」。
  /// Code Logic: mutation-operation 查 ledger → 刷新 worktrees → merge/collectMerge 再拉
  /// 主分支提交 → reconcileGitMutation 纯矩阵裁决 → 相位机落终态或保持 unknown。
  Future<GitMutationReconcile> _reconcile(String operationId) async {
    _tracker.beginReconcile();
    if (mounted) {
      setState(() {});
    }
    Map<String, dynamic>? ledger;
    try {
      ledger = await _client.mutationOperation(operationId);
    } catch (_) {
      ledger = null;
    }
    await _refreshWorktreesOnly();
    List<String>? mainCommitHashes;
    final intent = ledger?['intent'];
    final kind =
        parseGitMutationKind(intent is Map ? intent['kind'] as String? : null);
    if (kind == GitMutationKind.merge || kind == GitMutationKind.collectMerge) {
      final main = pickMainWorktree(_trees);
      final mainId = main?['id'] as String?;
      if (main != null && mainId != null && mainId.isNotEmpty) {
        try {
          final mainCommits =
              await _client.commits(widget.project.id, worktreeId: mainId, limit: 100);
          mainCommitHashes = [for (final commit in mainCommits) commit.hash];
        } catch (_) {
          mainCommitHashes = null;
        }
      }
    }
    return reconcileGitMutation(
      ledger: ledger,
      worktrees: _trees,
      mainCommitHashes: mainCommitHashes,
    );
  }

  /// unknown 相位的「重新对账」入口：复用同一 operationId，不盲重放。
  Future<void> _retryReconcile() async {
    final operationId = _tracker.operationId;
    if (operationId == null ||
        _tracker.phase != GitMutationPhase.unknown ||
        _busy != null) {
      return;
    }
    setState(() => _busy = '核对');
    try {
      final result = await _reconcile(operationId);
      _tracker.settleReconcile(result);
      if (!mounted) {
        return;
      }
      if (result == GitMutationReconcile.confirmedSucceeded) {
        _showSnack('已核对：操作已生效', transient: true);
        await _refresh();
      } else if (result == GitMutationReconcile.confirmedFailed) {
        _showSnack('操作失败，可以重新发起。');
      }
    } finally {
      if (mounted) {
        setState(() => _busy = null);
      }
    }
  }

  /// Business Logic: commit/push/pull/merge 的统一执行通道——busy 防重入、稳定 operation id、
  /// unknown 相位锁定 + 对账，确定性失败解锁并提示，hook 失败转修复卡。
  /// Code Logic: action 返回后端 envelope；succeeded → 成功提示+刷新；failedHook → 修复卡；
  /// unknown → 自动对账；传输异常（超时/断连）→ 进入 unknown 相位；其余异常 → 解锁+错误提示。
  Future<void> _runMutation(
    GitMutationKind kind,
    String label,
    Map<String, dynamic> tree,
    Future<Object?> Function(String operationId) action,
  ) async {
    // busy 或 unknown/reconciling 相位锁定全部动作；unknown 只能走「重新对账」。
    if (_busy != null || _tracker.actionLocked) {
      return;
    }
    final operationId = _tracker.begin(
      kind: kind,
      worktreeId: tree['id'] as String? ?? '',
      nextOperationId: newClientOperationId(),
    );
    setState(() => _busy = label);
    try {
      final envelope = GitMutationEnvelope.from(await action(operationId));
      if (envelope.succeeded) {
        _tracker.markIdle();
        if (kind == GitMutationKind.commit) {
          _showSnack('提交成功', transient: true);
        }
        await _refresh();
        return;
      }
      if (envelope.failedHook) {
        _tracker.markIdle();
        setState(() {
          _hookFailure = envelope.hookFailure ?? const {};
          _hookWorktreeId = tree['id'] as String?;
        });
        return;
      }
      if (envelope.unknown) {
        final result = await _reconcile(envelope.clientOperationId ?? operationId);
        _tracker.settleReconcile(result);
        if (!mounted) {
          return;
        }
        if (result == GitMutationReconcile.confirmedSucceeded) {
          if (kind == GitMutationKind.commit) {
            _showSnack('提交成功', transient: true);
          }
          await _refresh();
        } else if (result == GitMutationReconcile.confirmedFailed) {
          _showSnack('操作失败，可以重新发起。');
        }
        return;
      }
      _tracker.markIdle();
      _showSnack('$label 失败: 后端返回了未知的结果形态');
    } catch (error) {
      if (isTransportUnknownError(error)) {
        // 请求可能已到达也可能没到达：进入 unknown 相位等对账，禁止盲重试。
        _tracker.markUnknown();
      } else {
        _tracker.markIdle();
        if (mounted) {
          _showSnack('$label 失败: $error');
        }
      }
    } finally {
      if (mounted) {
        setState(() => _busy = null);
      }
    }
  }

  /// pull/push 前的确认框：说明对哪个 worktree 做什么。
  Future<bool> _confirmAction(String label, Map<String, dynamic> tree, String detail) async {
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: Text('确认$label'),
        content: Text(detail),
        actions: [
          TextButton(onPressed: () => Navigator.pop(context, false), child: const Text('取消')),
          FilledButton(onPressed: () => Navigator.pop(context, true), child: const Text('确认')),
        ],
      ),
    );
    return confirmed == true;
  }

  /// Business Logic: 功能 worktree merge 与主工作区 collect-merge 语义不同，
  /// 确认文案必须区分（对齐 web mergeConfirm / mergeCollectConfirm）。
  /// Code Logic: 主工作区列出可收集分支与 home 分支；非主 worktree 只说明合并目标。
  String mergeConfirmText(Map<String, dynamic> tree) {
    if (tree['isMain'] == true) {
      final branches = (tree['collectibleBranches'] as List?) ?? const [];
      final names = branches.whereType<String>().join(', ');
      final home = tree['homeBranch'] as String? ?? 'main';
      return '确定把本工作区的 ${branches.length} 条分支（$names）合并到「$home」，并切回该主分支？';
    }
    return '确定把「${worktreeDisplayName(tree)}」合并到主工作区？';
  }

  /// 提交前填写说明；留空则由后端 Claude Code 生成提交信息（对齐 web message=null）。
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
              decoration:
                  const InputDecoration(hintText: '留空则由 AI 生成提交信息'),
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
    if (message == null) {
      return;
    }
    await _runMutation(GitMutationKind.commit, '提交', tree, (operationId) {
      return _client.commit(
        worktreeId: id,
        clientOperationId: operationId,
        message: message.isEmpty ? null : message,
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
        _showSnack('已发起 hook AI 修复，请在终端查看进度。');
      }
    } catch (error) {
      if (mounted) {
        _showSnack('hook 修复失败: $error');
      }
    }
  }

  /// Business Logic: 当前选中 worktree 是否允许合并——非主 worktree 可合并回主工作区，
  /// 主工作区仅在存在可收集分支（canCollectMerge）时开放（对齐 web 按钮门控）。
  /// Code Logic: 布尔门控，缺失字段视为不可合并（宽容解析）。
  bool canMergeTree(Map<String, dynamic> tree) {
    if (tree['isMain'] != true) {
      return true;
    }
    return tree['canCollectMerge'] == true;
  }

  /// Business Logic: 「同步」要把当前仓库主分支推到 origin，再在其他设备的主工作区拉取；
  /// 无兄弟设备/主分支不可推/busy/锁定时必须禁用（对齐 web canSyncProjectMain）。
  /// Code Logic: 当前项目在全量清单里找同 fingerprint、异 deviceId 的兄弟，纯函数门控。
  bool get _canSync {
    final main = pickMainWorktree(_trees);
    final siblings = _siblings;
    return canSyncProjectMain(
      mainWorktree: main,
      siblingCount: siblings.length,
      busy: _busy != null,
      actionLocked: _tracker.actionLocked,
    );
  }

  List<Map<String, dynamic>> get _siblings {
    final currentId = widget.project.id;
    Map<String, dynamic>? current;
    for (final project in _allProjects) {
      if (project['id'] == currentId) {
        current = project;
        break;
      }
    }
    current ??= {'id': currentId, 'name': widget.project.name};
    return siblingProjectsOnOtherDevices(current, _allProjects);
  }

  String _projectDisplayName(Map<String, dynamic> project) {
    final name = project['deviceName'] as String? ?? project['name'] as String? ?? '';
    if (name.isNotEmpty) {
      return name;
    }
    return project['id'] as String? ?? '未知设备';
  }

  /// Business Logic: 同步 = push 本仓库主分支 → 逐个兄弟设备 list → pull 主 worktree，
  /// 汇总成功/部分失败摘要（对齐 web handleSync）。
  /// Code Logic: push/任一 pull 失败或 unknown 都不中断其余兄弟；完成后 SnackBar 摘要并刷新。
  Future<void> _sync() async {
    if (!_canSync) {
      return;
    }
    final main = pickMainWorktree(_trees);
    final mainId = main?['id'] as String?;
    if (main == null || mainId == null || mainId.isEmpty) {
      return;
    }
    final siblings = _siblings;
    setState(() => _busy = '同步');
    try {
      final pushEnvelope = GitMutationEnvelope.from(
        await _client.push(
          projectId: widget.project.id,
          worktreeId: mainId,
          clientOperationId: newClientOperationId(),
        ),
      );
      if (pushEnvelope.failedHook) {
        setState(() {
          _hookFailure = pushEnvelope.hookFailure ?? const {};
          _hookWorktreeId = mainId;
        });
        return;
      }
      if (pushEnvelope.unknown) {
        _showSnack('操作结果未知，请刷新后人工核对');
        return;
      }
      if (!pushEnvelope.succeeded) {
        _showSnack('同步主分支失败: 推送未成功');
        return;
      }
      final pulled = <String>[];
      final failed = <String>[];
      for (final sibling in siblings) {
        final name = _projectDisplayName(sibling);
        final siblingId = sibling['id'] as String? ?? '';
        if (siblingId.isEmpty) {
          failed.add(name);
          continue;
        }
        try {
          final body = await _client.listWorktrees(siblingId);
          final trees = asObjectList(body, wrapKey: 'worktrees');
          final siblingMain = pickMainWorktree(trees);
          final siblingMainId = siblingMain?['id'] as String?;
          if (siblingMain == null || siblingMainId == null || siblingMainId.isEmpty) {
            failed.add('$name: 拉取失败');
            continue;
          }
          final pullEnvelope = GitMutationEnvelope.from(
            await _client.pull(
              projectId: siblingId,
              worktreeId: siblingMainId,
              clientOperationId: newClientOperationId(),
            ),
          );
          if (pullEnvelope.succeeded) {
            pulled.add(name);
          } else {
            failed.add('$name: 拉取失败');
          }
        } catch (error) {
          failed.add('$name: $error');
        }
      }
      if (mounted) {
        _showSnack(syncSummaryText(pulled, failed));
      }
      await _refresh();
      await _loadProjects();
    } catch (error) {
      if (mounted) {
        _showSnack('同步主分支失败: $error');
      }
    } finally {
      if (mounted) {
        setState(() => _busy = null);
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    final selected = _selectedTree;
    final status = selected == null ? null : WorktreeGitStatus.of(selected);
    return RefreshIndicator(
      onRefresh: _refresh,
      child: ListView(
        physics: const AlwaysScrollableScrollPhysics(),
        padding: const EdgeInsets.fromLTRB(8, 8, 8, 24),
        children: [
          if (_busy != null) LinearProgressIndicator(key: ValueKey(_busy)),
          if (_tracker.phase == GitMutationPhase.reconciling)
            const Padding(
              padding: EdgeInsets.all(8),
              child: Text('核对结果中…'),
            ),
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
            if (selected != null && status != null)
              _buildStatusCard(selected, status),
            if (_trees.isEmpty && _error == null)
              const Padding(
                padding: EdgeInsets.all(24),
                child: Center(child: Text('没有 worktree。')),
              ),
            if (_tracker.phase == GitMutationPhase.unknown)
              Card(
                key: const Key('git-unknown-banner'),
                color: Theme.of(context).colorScheme.errorContainer,
                child: ListTile(
                  title: const Text('操作结果未知'),
                  subtitle: const Text('操作结果未知，请刷新后人工核对。'),
                  trailing: TextButton(
                    key: const Key('git-retry-reconcile'),
                    onPressed: _busy != null ? null : _retryReconcile,
                    child: const Text('重新对账'),
                  ),
                ),
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
                  key: Key('git-tree-${tree['id']}'),
                  selected: tree['id'] == (selected?['id']),
                  title: Text(worktreeDisplayName(tree)),
                  subtitle: Text(tree['branch'] as String? ?? ''),
                  onTap: () => setState(() => _localSelectedId = tree['id'] as String?),
                ),
              ),
            _buildCommitsSection(selected),
          ],
        ],
      ),
    );
  }

  /// 状态卡：当前 worktree 的分支、工作区状态、ahead/behind 与动作工具行
  /// （提交/拉取/推送/合并/同步，对齐 web 状态卡工具栏）。
  Widget _buildStatusCard(Map<String, dynamic> tree, WorktreeGitStatus status) {
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
            _statusRow('推送状态', status.canPush ? '可推送' : '不可推送'),
            const SizedBox(height: 8),
            Wrap(
              spacing: 8,
              runSpacing: 8,
              children: [
                FilledButton(
                  key: const Key('git-action-commit'),
                  onPressed: _actionDisabled ? null : () => _commit(tree),
                  child: const Text('提交'),
                ),
                OutlinedButton(
                  key: const Key('git-action-pull'),
                  onPressed: _actionDisabled
                      ? null
                      : () async {
                          final ok = await _confirmAction(
                            '拉取',
                            tree,
                            '将对 worktree「${worktreeDisplayName(tree)}」执行拉取。',
                          );
                          if (!ok) {
                            return;
                          }
                          await _runMutation(
                            GitMutationKind.pull,
                            '拉取',
                            tree,
                            (operationId) => _client.pull(
                              projectId: widget.project.id,
                              worktreeId: tree['id'] as String? ?? '',
                              clientOperationId: operationId,
                            ),
                          );
                        },
                  child: const Text('拉取'),
                ),
                OutlinedButton(
                  key: const Key('git-action-push'),
                  onPressed: _actionDisabled || !status.canPush
                      ? null
                      : () {
                          _runMutation(
                            GitMutationKind.push,
                            '推送',
                            tree,
                            (operationId) => _client.push(
                              projectId: widget.project.id,
                              worktreeId: tree['id'] as String? ?? '',
                              clientOperationId: operationId,
                            ),
                          );
                        },
                  child: const Text('推送'),
                ),
                OutlinedButton(
                  key: const Key('git-action-merge'),
                  onPressed: _actionDisabled || !canMergeTree(tree)
                      ? null
                      : () async {
                          final ok = await _confirmAction('合并', tree, mergeConfirmText(tree));
                          if (!ok) {
                            return;
                          }
                          await _runMutation(
                            GitMutationKind.merge,
                            '合并',
                            tree,
                            (operationId) => _client.merge(
                              projectId: widget.project.id,
                              worktreeId: tree['id'] as String? ?? '',
                              clientOperationId: operationId,
                            ),
                          );
                        },
                  child: const Text('合并'),
                ),
                OutlinedButton(
                  key: const Key('git-action-sync'),
                  onPressed: _canSync ? () => _sync() : null,
                  child: const Text('同步'),
                ),
              ],
            ),
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
