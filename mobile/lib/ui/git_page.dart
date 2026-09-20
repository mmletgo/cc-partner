import 'dart:async';

import 'package:flutter/material.dart';

import '../address_book/book.dart';
import '../core/lan_http.dart';
import '../git/client.dart';
import '../git/mutation.dart';
import '../git/project_sync.dart';
import '../projects/client.dart';
import '../terminal/git_actions.dart';
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
    this.onWorktreesMutated,
    this.onWorktreeOperationBusyChanged,
    this.confirmLeaveDirty,
    this.onFocusRepairSession,
  });

  final AddressBook book;
  final LanHttpClient http;
  final ProjectSummary project;
  final String? worktreeId;

  /// 测试可注入的 Git 客户端；缺省按当前 PC 地址簿构造。
  final GitClient? gitClient;

  /// merge 成功（含 unknown 对账确认成功）后通知壳层刷新权威 worktrees 列表
  /// （对齐 web GitPanel onMergeWorktree/onRefreshWorktrees 回写；固定接缝契约）。
  final VoidCallback? onWorktreesMutated;

  /// merge 全程（发起 → settle，含 unknown 对账）的壳层 worktree 操作互斥回调：
  /// true 置忙 / false 释放（try/finally 成对；多个来源并发由壳层计数收敛）。
  /// 壳层据此拒绝 worktree 切换并禁用 worktrees 页卡片（对齐 web
  /// beginWorktreeOperation 全局计数锁；固定接缝契约）。
  final ValueChanged<bool>? onWorktreeOperationBusyChanged;

  /// merge 的源 worktree 是当前激活 worktree 且 Files 有脏文件时的确认出口
  /// （GitPage 拿不到 FileWorkspaceController，由壳层注入 _confirmLeaveDirty；固定接缝契约）。
  /// 返回 false 时中止合并。
  final Future<bool> Function(String worktreeId)? confirmLeaveDirty;

  /// hook AI 修复返回的 terminalSessionId 回调：壳层切到终端面板聚焦修复会话
  /// （对齐 web MobileGitPanel onFocusRepairSession；固定接缝契约）。
  final void Function(String sessionId)? onFocusRepairSession;

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

  /// hook 失败的宽容视图（stdout/stderr/exitCode/stage），与 [_hookFailure] 同步写入；
  /// 修复接口仍回传原始载荷 [_hookFailure]。
  HookFailureView? _hookFailureView;
  String? _hookWorktreeId;
  List<WorkbenchGitCommit> _commits = [];
  bool _commitsLoading = false;
  String? _commitsError;

  /// 提交历史请求代数：选中树切换/刷新后自增，迟到的旧响应按代数丢弃，
  /// 避免旧 worktree 的提交历史挂到新选中树下（对齐 web MobileGitPanel requestIdRef）。
  int _commitsGeneration = 0;

  /// 用户在 Git 页内点选的 worktree（优先于 shell 传入的 worktreeId）。
  String? _localSelectedId;

  @override
  void initState() {
    super.initState();
    _client =
        widget.gitClient ?? GitClient(widget.http, widget.book.active!.baseUrl);
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
      final body = await _client.listWorktrees(
        widget.project.id,
        includeGitStatus: true,
      );
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
  ///
  /// Business Logic: 用户快速切换 worktree 卡时，旧树的提交请求可能晚于新树返回，
  /// 迟到响应不得覆盖当前选中树的提交历史（对齐 web requestIdRef 代数守卫）。
  /// Code Logic: 进入即自增请求代数并捕获；await 返回后代数不一致或已卸载则丢弃。
  Future<void> _loadCommits() async {
    final generation = ++_commitsGeneration;
    final tree = _selectedTree;
    final worktreeId = tree?['id'] as String?;
    if (tree == null || worktreeId == null || worktreeId.isEmpty) {
      if (!mounted || generation != _commitsGeneration) {
        return;
      }
      setState(() {
        _commits = [];
        _commitsError = null;
        _commitsLoading = false;
      });
      return;
    }
    if (mounted) {
      setState(() {
        _commitsLoading = true;
        _commitsError = null;
      });
    }
    try {
      final commits = await _client.commits(
        widget.project.id,
        worktreeId: worktreeId,
      );
      if (!mounted || generation != _commitsGeneration) {
        return;
      }
      setState(() {
        _commits = commits;
        _commitsLoading = false;
      });
    } catch (error) {
      if (!mounted || generation != _commitsGeneration) {
        return;
      }
      setState(() {
        _commitsError = error.toString();
        _commitsLoading = false;
      });
    }
  }

  void _showSnack(String message, {bool transient = false}) {
    ScaffoldMessenger.of(context).showSnackBar(
      SnackBar(
        content: Text(message),
        duration: transient
            ? kGitSuccessSnackbarDuration
            : const Duration(seconds: 4),
      ),
    );
  }

  /// Business Logic: failedHook envelope 要在页面上展示 hook 输出，同时修复接口需要原始载荷。
  /// Code Logic: 记录原始 hookFailure + 宽容解析出的 HookFailureView（stdout/stderr 拼接）+
  /// 目标 worktree id，供修复卡展示与 `_repairHook` 回传。
  void _showHookFailure(Map<String, dynamic> failure, String worktreeId) {
    setState(() {
      _hookFailure = failure;
      _hookFailureView = HookFailureView.fromJson(failure);
      _hookWorktreeId = worktreeId;
    });
  }

  /// Business Logic: 提交/推送/合并/拉取返回 unknown（网络异常等无法确定结果）时禁止盲重放，
  /// 必须用同一 clientOperationId 查 ledger + 权威列表对账出「实际已成功/未生效」。
  /// Code Logic: 共享 reconcileWorktreeMutation 通道（mutation-operation 查 ledger → 刷新 worktrees
  /// → merge/collectMerge 再拉主分支提交 → reconcileGitMutation 纯矩阵裁决）→ 相位机推进 reconciling。
  Future<GitMutationReconcile> _reconcile(String operationId) async {
    _tracker.beginReconcile();
    if (mounted) {
      setState(() {});
    }
    return reconcileWorktreeMutation(
      client: _client,
      projectId: widget.project.id,
      operationId: operationId,
      onTrees: (trees) {
        if (mounted) {
          setState(() => _trees = trees);
        }
      },
    );
  }

  /// Business Logic: unknown 相位的「重新对账」入口：复用同一 operationId，不盲重放。
  /// Code Logic: 共享 reconcileWorktreeMutation 通道（查 ledger + 刷新权威列表 + merge 取主分支
  /// 提交作 authority）→ 相位机落终态或保持 unknown；确认成功后回写壳层刷新 worktrees。
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
        widget.onWorktreesMutated?.call();
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
  /// Code Logic: action 返回后端 envelope；succeeded → 成功提示+刷新+回写壳层刷新
  /// 权威 worktrees（merge 原有行为不变；对齐 web refreshAfterAction 对全部动作调
  /// onRefreshWorktrees，壳层 _handleWorktreesMutated 为幂等收敛入口）；merge 全程
  /// 回调壳层置忙（try/finally 成对释放）；failedHook → 修复卡；unknown → 自动对账，
  /// 确认成功同样回写；传输异常（超时/断连）→ 进入 unknown 相位；其余异常 → 解锁+错误提示。
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
    // merge 会删除源 worktree：发起即通知壳层置忙，禁止在途期间切换 worktree
    // （对齐 web handleMergeWorktree 的 beginWorktreeOperation 全程持锁）。
    final isMerge = kind == GitMutationKind.merge;
    if (isMerge) {
      widget.onWorktreeOperationBusyChanged?.call(true);
    }
    setState(() => _busy = label);
    try {
      final envelope = GitMutationEnvelope.from(await action(operationId));
      if (envelope.succeeded) {
        _tracker.markIdle();
        if (kind == GitMutationKind.commit) {
          _showSnack('提交成功', transient: true);
        }
        await _refresh();
        // commit/pull/push/merge 全部回写壳层收敛权威列表：壳层 strip/状态行/
        // 终端合并门控依赖该回调保持新鲜（对齐 web refreshAfterAction）。
        widget.onWorktreesMutated?.call();
        return;
      }
      if (envelope.failedHook) {
        _tracker.markIdle();
        _showHookFailure(
          envelope.hookFailure ?? const {},
          tree['id'] as String? ?? '',
        );
        return;
      }
      if (envelope.unknown) {
        final result = await _reconcile(
          envelope.clientOperationId ?? operationId,
        );
        _tracker.settleReconcile(result);
        if (!mounted) {
          return;
        }
        if (result == GitMutationReconcile.confirmedSucceeded) {
          if (kind == GitMutationKind.commit) {
            _showSnack('提交成功', transient: true);
          }
          await _refresh();
          // 对账确认成功同样回写壳层（对齐 web 对账成功后 refreshAfterAction）。
          widget.onWorktreesMutated?.call();
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
      // merge 置忙的成对释放：成功/失败/unknown 一律在 finally 释放（对齐 web
      // endWorktreeOperation 的幂等 finally 语义）。
      if (isMerge) {
        widget.onWorktreeOperationBusyChanged?.call(false);
      }
      if (mounted) {
        setState(() => _busy = null);
      }
    }
  }

  /// pull/push 前的确认框：说明对哪个 worktree 做什么。
  Future<bool> _confirmAction(
    String label,
    Map<String, dynamic> tree,
    String detail,
  ) async {
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: Text('确认$label'),
        content: Text(detail),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context, false),
            child: const Text('取消'),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(context, true),
            child: const Text('确认'),
          ),
        ],
      ),
    );
    return confirmed == true;
  }

  /// Business Logic: 功能 worktree merge 与主工作区 collect-merge 语义不同，
  /// 确认文案必须区分（对齐 web mergeConfirm / mergeCollectConfirm）。
  /// Code Logic: 委托共享 worktreeMergeConfirmText（终端页 / worktrees 页同一口径）。
  String mergeConfirmText(Map<String, dynamic> tree) =>
      worktreeMergeConfirmText(tree);

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
              decoration: const InputDecoration(hintText: '留空则由 AI 生成提交信息'),
            ),
          ],
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context),
            child: const Text('取消'),
          ),
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

  /// 触发 hook AI 修复；成功清卡片并提示，返回 terminalSessionId 时回调壳层聚焦修复终端
  /// （对齐 web MobileGitPanel：读 repair.terminalSessionId → onFocusRepairSession 刷新并切面板）。
  ///
  /// Business Logic: 修复请求在 owning device 启动可见 Claude agent，进行中必须禁用入口
  /// 防重复触发（对齐 web MobileHookRepairCard busy → runButtonBusy + disabled）。
  /// Code Logic: busy 复用页级 _busy（联动 _actionDisabled 锁全部动作）；结束后在 finally 解锁。
  Future<void> _repairHook() async {
    final failure = _hookFailure;
    if (failure == null || _busy != null) {
      return;
    }
    setState(() => _busy = '修复');
    try {
      final result = await _client.repairHookFailure(
        worktreeId: _hookWorktreeId ?? widget.worktreeId ?? '',
        hookFailure: failure,
      );
      final terminalSessionId =
          result['terminalSessionId'] as String? ??
          result['terminal_session_id'] as String?;
      if (mounted) {
        _dismissHookFailure();
      }
      if (terminalSessionId != null && terminalSessionId.isNotEmpty) {
        // 壳层负责刷新 sessions、切终端面板并聚焦该会话（B4 接缝契约）。
        widget.onFocusRepairSession?.call(terminalSessionId);
      } else if (mounted) {
        _showSnack('已发起 hook AI 修复，请在终端查看进度。');
      }
    } catch (error) {
      if (mounted) {
        _showSnack('hook 修复失败: $error');
      }
    } finally {
      if (mounted) {
        setState(() => _busy = null);
      }
    }
  }

  /// Business Logic: 用户决定不修时必须能清掉过期 failedHook 卡；仅本地清除不调后端
  /// （对齐 web MobileHookRepairCard dismissButton 语义）。
  /// Code Logic: 置空原始载荷、宽容视图与目标 worktree id 三元状态。
  void _dismissHookFailure() {
    setState(() {
      _hookFailure = null;
      _hookFailureView = null;
      _hookWorktreeId = null;
    });
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
    final name =
        project['deviceName'] as String? ?? project['name'] as String? ?? '';
    if (name.isNotEmpty) {
      return name;
    }
    return project['id'] as String? ?? '未知设备';
  }

  /// Business Logic: 同步 = push 本仓库主分支 → 逐个兄弟设备 list → pull 主 worktree，
  /// 汇总成功/部分失败摘要（对齐 web handleSync）。push 是会产生远端副作用的 mutation：
  /// unknown（envelope 或传输异常）时必须纳入相位机锁定，靠同一 clientOperationId 对账解锁，
  /// 禁止盲重放；兄弟设备 pull 阶段的失败不锁定，仍走摘要 SnackBar。
  /// Code Logic: push 前用 `_tracker.begin(kind: push)` 拿稳定 operation id；push unknown →
  /// 复用 `_reconcile`（共享 reconcileWorktreeMutation 通道）自动对账：确认成功 → 继续兄弟
  /// 拉取；确认失败 → 提示解锁；仍 unknown → 保持 `git-unknown-banner` + 「重新对账」；
  /// 传输异常 → `_tracker.markUnknown` 等手动对账。pull 循环逐设备捕获异常不中断。
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
    final operationId = _tracker.begin(
      kind: GitMutationKind.push,
      worktreeId: mainId,
      nextOperationId: newClientOperationId(),
    );
    setState(() => _busy = '同步');
    try {
      final pushEnvelope = GitMutationEnvelope.from(
        await _client.push(
          projectId: widget.project.id,
          worktreeId: mainId,
          clientOperationId: operationId,
        ),
      );
      if (pushEnvelope.failedHook) {
        _tracker.markIdle();
        _showHookFailure(pushEnvelope.hookFailure ?? const {}, mainId);
        return;
      }
      if (pushEnvelope.unknown) {
        // 与 commit/push/merge 同通道：先自动对账；仍不确定则锁定等「重新对账」。
        final result = await _reconcile(
          pushEnvelope.clientOperationId ?? operationId,
        );
        _tracker.settleReconcile(result);
        if (!mounted) {
          return;
        }
        if (result == GitMutationReconcile.confirmedFailed) {
          _showSnack('同步主分支失败: 推送未生效，可重新发起');
          return;
        }
        if (result != GitMutationReconcile.confirmedSucceeded) {
          return;
        }
      } else if (pushEnvelope.succeeded) {
        _tracker.markIdle();
      } else {
        _tracker.markIdle();
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
          if (siblingMain == null ||
              siblingMainId == null ||
              siblingMainId.isEmpty) {
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
      // 同步完成（含部分兄弟失败）也回写壳层收敛权威列表（对齐 web
      // handleSync 尾部的 refreshAfterAction('sync')）。
      widget.onWorktreesMutated?.call();
    } catch (error) {
      if (isTransportUnknownError(error)) {
        // push 请求可能已到达也可能没到达：进入 unknown 相位等对账，禁止盲重试。
        _tracker.markUnknown();
      } else {
        _tracker.markIdle();
        if (mounted) {
          _showSnack('同步主分支失败: $error');
        }
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
            const Padding(padding: EdgeInsets.all(8), child: Text('核对结果中…')),
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
                  trailing: TextButton(
                    onPressed: _reload,
                    child: const Text('重试'),
                  ),
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
            if (_hookFailure != null) _buildHookFailureCard(context),
            for (final tree in _trees)
              Card(
                child: ListTile(
                  key: Key('git-tree-${tree['id']}'),
                  selected: tree['id'] == (selected?['id']),
                  title: Text(worktreeDisplayName(tree)),
                  subtitle: Text(tree['branch'] as String? ?? ''),
                  onTap: () {
                    setState(() => _localSelectedId = tree['id'] as String?);
                    // 提交历史立即跟随新选中树（对齐 web useEffect(project/worktree) 驱动）；
                    // 请求代数保证旧树迟到的响应不会覆盖新树的历史。
                    _loadCommits();
                  },
                ),
              ),
            _buildCommitsSection(selected),
          ],
        ],
      ),
    );
  }

  /// Business Logic: hook 失败要给用户看得到 stdout/stderr 输出与退出码，
  /// 交互与文案对齐 web MobileHookRepairCard / 终端页 hook 修复卡：
  /// 标题区分 pre-commit/pre-push 阶段，「让 AI 修复」busy 时禁用并显示进行中文案，
  /// 「忽略」仅本地清卡；可展开/收起输出，空输出给占位。
  /// Code Logic: 复用 HookFailureView 宽容解析（camel/snake 双读）与 formattedOutput
  /// 拼接；修复按钮复用页级 _actionDisabled（busy/对账相位期间锁定）。
  Widget _buildHookFailureCard(BuildContext context) {
    final theme = Theme.of(context);
    final failure = _hookFailureView ?? HookFailureView.fromJson(_hookFailure);
    return Card(
      color: theme.colorScheme.errorContainer,
      child: Padding(
        padding: const EdgeInsets.fromLTRB(16, 12, 16, 12),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              children: [
                Expanded(
                  child: Text(
                    // 对齐 web hookRepair.titleCommit/titlePush（与终端页同口径）。
                    failure.isPush
                        ? 'pre-push 钩子阻止 push'
                        : 'pre-commit 钩子阻止 commit',
                    style: theme.textTheme.titleMedium,
                  ),
                ),
                if (failure.exitCode != null)
                  Text(
                    '退出码 ${failure.exitCode}',
                    style: theme.textTheme.bodySmall,
                  ),
              ],
            ),
            _HookOutputToggle(output: failure.formattedOutput),
            Wrap(
              spacing: 8,
              children: [
                FilledButton(
                  key: const Key('git-hook-repair'),
                  onPressed: _actionDisabled ? null : _repairHook,
                  child: Text(_actionDisabled ? '正在启动 AI 修复…' : '让 AI 修复'),
                ),
                TextButton(
                  key: const Key('git-hook-dismiss'),
                  onPressed: _dismissHookFailure,
                  child: const Text('忽略'),
                ),
              ],
            ),
          ],
        ),
      ),
    );
  }

  /// 状态卡：当前 worktree 的分支、工作区状态、ahead/behind 与动作工具行
  /// （提交/拉取/推送/合并/同步，对齐 web 状态卡工具栏）。
  Widget _buildStatusCard(Map<String, dynamic> tree, WorktreeGitStatus status) {
    final branch = status.branch ?? tree['branch'] as String? ?? '—';
    // 三态口径与 worktrees 页共用 worktreeStatusLabel（N 处冲突 / N 处改动 / 干净）；
    // 后端未返回 status 时保留「状态未知」提示。
    final stateText = status.present ? worktreeStatusLabel(tree) : '状态未知';
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
                          final ok = await _confirmAction(
                            '合并',
                            tree,
                            mergeConfirmText(tree),
                          );
                          if (!ok) {
                            return;
                          }
                          // B3 接缝契约：merge 的源 worktree 是当前激活 worktree 时，
                          // Files 的未保存改动可能随合并切走上下文，先过壳层 dirty guard。
                          final guard = widget.confirmLeaveDirty;
                          final sourceId = tree['id'] as String? ?? '';
                          if (guard != null &&
                              sourceId.isNotEmpty &&
                              sourceId == widget.worktreeId) {
                            final allowed = await guard(sourceId);
                            if (!allowed) {
                              return;
                            }
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
                child: Text(
                  '最近提交',
                  style: Theme.of(context).textTheme.titleMedium,
                ),
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
              trailing: TextButton(
                onPressed: _loadCommits,
                child: const Text('重试'),
              ),
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
                    style: Theme.of(context).textTheme.bodyMedium?.copyWith(
                      fontWeight: FontWeight.w600,
                    ),
                  ),
                ),
                const SizedBox(width: 8),
                Text(
                  commit.shortHash.isEmpty ? '—' : commit.shortHash,
                  style: Theme.of(
                    context,
                  ).textTheme.bodySmall?.copyWith(fontFamily: 'monospace'),
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

/// 可展开的 hook 输出区（「展开钩子输出 / 收起钩子输出」，空输出给占位文案）。
///
/// Business Logic（为什么需要）:
///   Git 页 hook 失败卡要与终端页同一交互口径：默认收起避免刷屏，用户按需展开
///   查看 stdout/stderr；没有输出时给明确占位而不是空白。
///
/// Code Logic（做什么）:
///   本地 _expanded 状态切换按钮文案与输出区显隐；终端页的同类组件是私有的，
///   跨文件不可复用，此处按同一形态在页内落地。
class _HookOutputToggle extends StatefulWidget {
  const _HookOutputToggle({required this.output});

  final String output;

  @override
  State<_HookOutputToggle> createState() => _HookOutputToggleState();
}

class _HookOutputToggleState extends State<_HookOutputToggle> {
  bool _expanded = false;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        TextButton(
          onPressed: () => setState(() => _expanded = !_expanded),
          child: Text(_expanded ? '收起钩子输出' : '展开钩子输出'),
        ),
        if (_expanded)
          Container(
            width: double.infinity,
            padding: const EdgeInsets.all(8),
            color: theme.colorScheme.surface,
            child: Text(
              widget.output.isEmpty ? '（未捕获到输出）' : widget.output,
              style: theme.textTheme.bodySmall?.copyWith(
                fontFamily: 'monospace',
              ),
            ),
          ),
      ],
    );
  }
}
