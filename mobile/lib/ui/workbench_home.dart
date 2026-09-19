import 'dart:async';

import 'package:flutter/material.dart';

import '../address_book/book.dart';
import '../address_book/models.dart';
import '../attention/client.dart';
import '../attention/filter.dart';
import '../core/lan_http.dart';
import '../files/workspace.dart';
import '../git/client.dart';
import '../git/mutation.dart';
import '../projects/client.dart';
import '../sessions/client.dart';
import '../transfer/api.dart';
import '../workbench/nav.dart';
import '../workbench/worktree.dart';
import 'attention_page.dart';
import 'automation_page.dart';
import 'browser_page.dart';
import 'files_page.dart';
import 'git_page.dart';
import 'projects_page.dart';
import 'provider_page.dart';
import 'settings_page.dart';
import 'terminal_page.dart';
import 'transfer_page.dart';
import 'workbench_shell.dart';
import 'worktree_strip.dart';
import 'worktrees_page.dart';

/// Dual-mode workbench: global 项目/待处理/传输/设置/Provider,
/// project 终端/浏览器/文件/Git/worktrees/自动化.
class WorkbenchHome extends StatefulWidget {
  const WorkbenchHome({super.key, required this.book, required this.http});

  final AddressBook book;
  final LanHttpClient http;

  @override
  State<WorkbenchHome> createState() => _WorkbenchHomeState();
}

class _WorkbenchHomeState extends State<WorkbenchHome> {
  WorkbenchPanel _panel = WorkbenchPanel.projects;
  ProjectSummary? _project;
  String? _worktreeId;
  String? _sessionId;
  List<Map<String, dynamic>> _worktrees = [];
  final _files = FileWorkspaceController();
  late final GitClient _gitClient =
      GitClient(widget.http, widget.book.active!.baseUrl);

  /// 「待处理」未读徽章（与列表同口径：只统计今天未读）。
  int _attentionUnread = 0;

  /// 内测开关（对齐 web ExperimentalFeaturesProvider：失败 fail-closed 全关）。
  bool _automationEnabled = false;
  bool _browserEnabled = false;

  /// Attention 跳转 Automation 时要聚焦的任务/发件箱 id（接缝契约）。
  String? _attentionFocusTaskId;
  String? _attentionFocusOutboxId;

  /// 终端是否全屏（全屏时隐藏 worktree 切换条）。
  bool _terminalFullscreen = false;

  /// strip 删除 worktree 进行中（防重复提交）。
  bool _removingTree = false;

  /// strip 创建 worktree 进行中（防重复提交）。
  bool _creatingTree = false;

  /// strip 删除 mutation 相位机：unknown 后锁定删除并要求同 id 对账（对齐 web bar controller）。
  final GitMutationTracker _stripMutation = GitMutationTracker();

  /// strip mutation 结果未知等错误文案（条上错误条展示，unknown 相位带「重新对账」）。
  String? _stripMutationError;

  ServerRecord get _server => widget.book.active!;

  WorkbenchNavMode get _mode => resolveNavMode(_panel, _project != null);

  @override
  void initState() {
    super.initState();
    unawaited(_refreshAttentionUnread());
    unawaited(_restoreLastLocation());
    unawaited(_loadExperimentalFeatures());
  }

  /// Business Logic: automation/browser 是内测开关，进入工作台时要读同一份权威开关
  /// （对齐 web ExperimentalFeaturesProvider 读 GET /api/orchestrator/config 的
  /// experimentalFeatures；缺字段/失败 fail-closed 全关）。
  /// Code Logic: 宽容解析 automation/browser 两个布尔；加载后若当前面板已被关闭则自动回落。
  Future<void> _loadExperimentalFeatures() async {
    try {
      final body = await widget.http.getJson(
        _server.baseUrl,
        '/api/orchestrator/config',
      );
      final features = body['experimentalFeatures'];
      if (!mounted) {
        return;
      }
      setState(() {
        _automationEnabled = features is Map && features['automation'] == true;
        _browserEnabled = features is Map && features['browser'] == true;
      });
      _collapseClosedPanel();
    } catch (_) {
      // fail-closed：保持全关。
    }
  }

  /// Business Logic: 用户停在 automation/browser 时开关被关闭（或加载后未开），
  /// 必须收回可用面板，不能留在黑屏页（对齐 web MobileWorkbench 收回 effect）。
  /// Code Logic: 纯函数回落——有项目 → terminal，无项目 → projects；面板未关闭则不动。
  void _collapseClosedPanel() {
    final resolved = resolvePanelForFeatures(
      panel: _panel,
      hasProject: _project != null,
      automationEnabled: _automationEnabled,
      browserEnabled: _browserEnabled,
    );
    if (resolved != _panel && mounted) {
      setState(() => _panel = resolved);
    }
  }

  /// Business Logic: 进入工作台时 Drawer「待处理」要显示未读数，且数字必须与列表一致。
  /// Code Logic: 拉取移动端可见条目，按 filter.dart 的本地日口径统计今天未读；离线失败静默保留旧值。
  Future<void> _refreshAttentionUnread() async {
    try {
      final items = await AttentionClient(widget.http, _server.baseUrl).listVisible();
      if (!mounted) {
        return;
      }
      setState(() {
        _attentionUnread = countTodayUnreadAttentionItems(items, DateTime.now());
      });
    } catch (_) {}
  }

  /// Business Logic: 离开工作台回地址簿后，再次进入同一 PC 应恢复上次的项目/面板/worktree/session。
  /// Code Logic: 弹出路由时把当前工作位置写入该 server 的 lastLocation 并持久化；未打开项目则不覆盖。
  Future<void> _saveLastLocation() async {
    final project = _project;
    if (project == null) {
      return;
    }
    await widget.book.saveLastLocation(
      _server.id,
      LastLocation(
        projectId: project.id,
        panel: _panel.name,
        worktreeId: _worktreeId,
        sessionId: _sessionId,
      ),
    );
  }

  /// Business Logic: 再次进入该 PC 工作台时应尽量回到上次的工作位置。
  /// Code Logic: 取最近项目列表后用纯函数回落解析（项目不在列表/无记录则保持现状）；
  /// worktree 用 resumeWorktreeId 在新鲜列表里校验（无效回落主树）；session 由 TerminalPage 自行回落。
  Future<void> _restoreLastLocation() async {
    final location = _server.lastLocation;
    try {
      final projects = await ProjectsClient(widget.http, _server.baseUrl).listRecent();
      final restore = resolveWorkbenchLocationRestore(
        location: location,
        recentProjectIds: projects.map((p) => p.id).toSet(),
      );
      if (restore == null || !mounted) {
        return;
      }
      final project = projects.firstWhere((p) => p.id == restore.projectId);
      _openProject(
        project,
        panel: restore.panel,
        sessionId: restore.sessionId,
        resumeWorktreeId: restore.worktreeId,
      );
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(
          const SnackBar(content: Text('已恢复上次的工作位置')),
        );
      }
    } catch (_) {}
  }

  /// Business Logic: 终端/文件/Git 与 worktrees 页都要看运行期 Git 状态
  /// （状态点/badge/可推送），列表统一带 includeGitStatus 拉取。
  Future<void> _loadWorktrees(
    ProjectSummary project, {
    required bool projectChanged,
    String? resumeWorktreeId,
  }) async {
    try {
      final body = await _gitClient.listWorktrees(project.id, includeGitStatus: true);
      final trees = asObjectList(body, wrapKey: 'worktrees');
      if (!mounted) {
        return;
      }
      setState(() {
        _worktrees = trees;
        _worktreeId = resumeWorktreeId != null
            ? resolveActiveWorktreeId(
                trees: trees,
                previousId: resumeWorktreeId,
                projectChanged: false,
              )
            : resolveActiveWorktreeId(
                trees: trees,
                previousId: _worktreeId,
                projectChanged: projectChanged,
              );
      });
    } catch (_) {}
  }

  Future<bool> _confirmLeaveDirty(String nextWorktreeId) async {
    if (!_files.shouldBlockContextSwitch(
      projectId: _project?.id ?? '',
      worktreeId: nextWorktreeId,
    )) {
      return true;
    }
    final choice = await showDialog<String>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('未保存的文件'),
        content: const Text('切换工作区前请保存或丢弃当前文件。'),
        actions: [
          TextButton(onPressed: () => Navigator.pop(context, 'cancel'), child: const Text('取消')),
          TextButton(onPressed: () => Navigator.pop(context, 'discard'), child: const Text('丢弃')),
        ],
      ),
    );
    if (choice == 'discard') {
      _files.markClean();
      return true;
    }
    return false;
  }

  /// Business Logic: worktrees 页/切换条选中新 worktree 后的统一入口：
  /// dirty 确认 → 写入 worktreeId；goTerminal 决定是否自动进入终端面板
  /// （对齐 web：点击 worktree 卡片切换后自动进入终端）。
  /// Code Logic: dirty guard 不过则放弃；同树只更新选中，跨树刷新列表并回落 active。
  Future<void> _selectWorktree(
    Map<String, dynamic> tree, {
    bool goTerminal = false,
  }) async {
    final id = tree['id'] as String? ?? '';
    if (id.isEmpty) {
      return;
    }
    if (!await _confirmLeaveDirty(id)) {
      return;
    }
    if (!mounted) {
      return;
    }
    setState(() {
      _worktreeId = id;
      if (goTerminal) {
        _panel = WorkbenchPanel.terminal;
      }
    });
    final project = _project;
    if (project != null) {
      unawaited(_loadWorktrees(project, projectChanged: false, resumeWorktreeId: id));
    }
  }

  /// Business Logic: 切换条上非主 chip 的 X 要能就地移除 worktree；破坏性操作必须先确认风险。
  /// 删除在 unknown（envelope 或传输异常）时禁止盲重放，必须用同一 clientOperationId
  /// 查 ledger + 权威列表对账（对齐 web useMobileWorktreeBarController confirmRemove）。
  /// Code Logic: 确认框 → tracker.begin(remove) 锁定 → remove envelope：
  ///   succeeded → 刷新+提示；unknown → 共享对账通道（成功后 _loadWorktrees 兜底回落 active）；
  ///   传输异常 → 条上错误条 +「重新对账」；服务器应答的确定失败 → 解锁 + SnackBar。
  Future<void> _removeTreeFromStrip(Map<String, dynamic> tree) async {
    if (_removingTree || _stripMutation.actionLocked) {
      return;
    }
    final id = tree['id'] as String? ?? '';
    final name = worktreeDisplayName(tree);
    if (id.isEmpty) {
      return;
    }
    final project = _project;
    if (project == null) {
      return;
    }
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
    if (confirmed != true || !mounted) {
      return;
    }
    final operationId = _stripMutation.begin(
      kind: GitMutationKind.remove,
      worktreeId: id,
      nextOperationId: newClientOperationId(),
    );
    setState(() {
      _removingTree = true;
      _stripMutationError = null;
    });
    try {
      final envelope = GitMutationEnvelope.from(
        await _gitClient.remove(worktreeId: id, clientOperationId: operationId),
      );
      if (envelope.succeeded) {
        _stripMutation.markIdle();
        await _loadWorktrees(project, projectChanged: false);
        _showStripSnack('已移除 worktree「$name」');
        return;
      }
      if (envelope.unknown) {
        await _reconcileStripRemoval(
          envelope.clientOperationId ?? operationId,
          project,
          successMessage: '已移除 worktree「$name」',
        );
        return;
      }
      _stripMutation.markIdle();
      _showStripSnack('移除失败: 后端返回了未知的结果形态');
    } catch (error) {
      if (!mounted) {
        return;
      }
      if (isTransportUnknownError(error)) {
        // 请求可能已到达也可能没到达：进入 unknown 相位等对账，禁止盲重试。
        _stripMutation.markUnknown();
        setState(() => _stripMutationError = '移除结果未知，请重新对账。');
      } else {
        _stripMutation.markIdle();
        _showStripSnack('移除失败: $error');
      }
    } finally {
      if (mounted) {
        setState(() => _removingTree = false);
      }
    }
  }

  /// Business Logic: strip unknown 相位「重新对账」：复用同一 operationId 查 ledger +
  /// 权威列表裁决；确认成功后 _loadWorktrees 兜底回落 active（源树被删时回主树/首项）。
  /// Code Logic: reconcileWorktreeMutation（共享通道）→ settleReconcile →
  ///   succeeded → 清错误条 + 提示；failed → 清错误条 + 失败提示；unknown → 保持错误条。
  Future<void> _reconcileStripRemoval(
    String operationId,
    ProjectSummary project, {
    String? successMessage,
  }) async {
    final result = await reconcileWorktreeMutation(
      client: _gitClient,
      projectId: project.id,
      operationId: operationId,
    );
    _stripMutation.settleReconcile(result);
    await _loadWorktrees(project, projectChanged: false);
    if (!mounted) {
      return;
    }
    if (result == GitMutationReconcile.confirmedSucceeded) {
      setState(() => _stripMutationError = null);
      if (successMessage != null) {
        _showStripSnack(successMessage);
      }
    } else if (result == GitMutationReconcile.confirmedFailed) {
      setState(() => _stripMutationError = null);
      _showStripSnack('移除失败：操作未生效，可以重新发起。');
    } else {
      setState(() => _stripMutationError = '移除结果未知，请重新对账。');
    }
  }

  /// strip 错误条上的「重新对账」入口（unknown 相位专用，防重复提交）。
  Future<void> _retryStripReconcile() async {
    final operationId = _stripMutation.operationId;
    final project = _project;
    if (operationId == null ||
        project == null ||
        _stripMutation.phase != GitMutationPhase.unknown ||
        _removingTree) {
      return;
    }
    setState(() => _removingTree = true);
    try {
      await _reconcileStripRemoval(operationId, project);
    } finally {
      if (mounted) {
        setState(() => _removingTree = false);
      }
    }
  }

  void _showStripSnack(String message) {
    if (!mounted) {
      return;
    }
    ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(message)));
  }

  /// Business Logic: 切换条要能就地新建 worktree 并自动开绑定终端（对齐 web MobileWorktreeTabs
  /// 条上创建表单）；与 worktrees 页共用 createWorktreeWithTerminalSession 执行通道。
  /// Code Logic: 组好的分支名 → 共享创建（create 失败提示；session 失败保留 worktree 只提示）→
  /// 刷新列表 → onSelect(created, goTerminal) 切 active 并进终端面板。
  Future<void> _createTreeFromStrip(String branchName) async {
    final project = _project;
    if (project == null || _creatingTree) {
      return;
    }
    setState(() => _creatingTree = true);
    final result = await createWorktreeWithTerminalSession(
      git: _gitClient,
      sessions: SessionsClient(widget.http, _server.baseUrl),
      projectId: project.id,
      branchName: branchName,
    );
    if (!mounted) {
      return;
    }
    if (result.createError != null) {
      setState(() => _creatingTree = false);
      _showStripSnack('创建失败: ${result.createError}');
      return;
    }
    await _loadWorktrees(project, projectChanged: false);
    if (!mounted) {
      return;
    }
    setState(() => _creatingTree = false);
    if (result.sessionError != null) {
      _showStripSnack('终端窗口创建失败（worktree 已保留）: ${result.sessionError}');
    } else {
      _showStripSnack('已创建 worktree「$branchName」');
    }
    final created = result.created;
    final newId = created?['id'] as String?;
    if (created != null && newId != null && newId.isNotEmpty) {
      await _selectWorktree(created, goTerminal: true);
    }
  }

  /// 当前激活 worktree 的权威 DTO（供 TerminalPage 合并门控/确认文案/Prompt 优化目录使用）。
  Map<String, dynamic>? get _currentWorktreeInfo {
    final id = _worktreeId;
    if (id == null) {
      return null;
    }
    for (final tree in _worktrees) {
      if (tree['id'] == id) {
        return tree;
      }
    }
    return null;
  }

  void _openProject(
    ProjectSummary project, {
    WorkbenchPanel panel = WorkbenchPanel.terminal,
    String? sessionId,
    String? resumeWorktreeId,
  }) {
    final projectChanged = _project?.id != project.id;
    // 恢复/跳转目标面板若被内测开关关闭，则回落到可用面板。
    final gated = resolvePanelForFeatures(
      panel: panel,
      hasProject: true,
      automationEnabled: _automationEnabled,
      browserEnabled: _browserEnabled,
    );
    setState(() {
      _project = project;
      _panel = gated;
      _sessionId = sessionId;
      if (projectChanged) {
        _worktreeId = clearWorktreeOnLeaveProject();
        _worktrees = [];
        // 切项目后旧 mutation unknown 锁不得污染新上下文（对齐 web 重置 effect）。
        _stripMutation.reset();
        _stripMutationError = null;
      }
    });
    _loadWorktrees(
      project,
      projectChanged: projectChanged,
      resumeWorktreeId: resumeWorktreeId,
    );
  }

  void _select(WorkbenchPanel next) {
    _attentionFocusTaskId = null;
    _attentionFocusOutboxId = null;
    final panel = selectPanelForProject(hasProject: _project != null, next: next);
    final gated = resolvePanelForFeatures(
      panel: panel,
      hasProject: _project != null,
      automationEnabled: _automationEnabled,
      browserEnabled: _browserEnabled,
    );
    setState(() => _panel = gated);
  }

  Widget _body() {
    switch (_panel) {
      case WorkbenchPanel.projects:
        return ProjectsPage(
          book: widget.book,
          http: widget.http,
          onOpen: _openProject,
          onProjectRemoved: _onProjectRemoved,
        );
      case WorkbenchPanel.attention:
        return AttentionPage(
          book: widget.book,
          http: widget.http,
          onNavigate: (
            project,
            panel,
            sessionId, {
            String? focusTaskId,
            String? focusOutboxId,
          }) {
            setState(() {
              _attentionFocusTaskId = focusTaskId;
              _attentionFocusOutboxId = focusOutboxId;
            });
            _openProject(
              project,
              panel: parseWorkbenchPanel(panel) ?? WorkbenchPanel.terminal,
              sessionId: sessionId,
            );
          },
          onItemsChanged: (items) {
            setState(() {
              _attentionUnread = countTodayUnreadAttentionItems(items, DateTime.now());
            });
          },
        );
      case WorkbenchPanel.transfer:
        return TransferPage(book: widget.book, http: widget.http);
      case WorkbenchPanel.settings:
        return SettingsPage(book: widget.book, http: widget.http);
      case WorkbenchPanel.provider:
        return ProviderPage(book: widget.book, http: widget.http);
      case WorkbenchPanel.terminal:
        final worktreeInfo = _currentWorktreeInfo;
        return TerminalPage(
          key: ValueKey('terminal-${_project!.id}-$_worktreeId-$_sessionId'),
          book: widget.book,
          http: widget.http,
          project: _project!,
          preferredSessionId: _sessionId,
          worktreeId: _worktreeId,
          worktreeInfo: worktreeInfo,
          worktreePath: worktreeInfo?['path'] as String?,
          onFullscreenChanged: (fullscreen) {
            setState(() => _terminalFullscreen = fullscreen);
          },
          onWorktreesMutated: () {
            final project = _project;
            if (project != null) {
              unawaited(_loadWorktrees(project, projectChanged: false));
            }
          },
        );
      case WorkbenchPanel.files:
        return FilesPage(
          key: ValueKey('files-${_project!.id}-$_worktreeId'),
          book: widget.book,
          http: widget.http,
          project: _project!,
          worktreeId: _worktreeId,
          workspace: _files,
        );
      case WorkbenchPanel.git:
        return GitPage(
          key: ValueKey('git-${_project!.id}-$_worktreeId'),
          book: widget.book,
          http: widget.http,
          project: _project!,
          worktreeId: _worktreeId,
          // B3 接缝契约：merge 成功后回写壳层刷新权威 worktrees 列表。
          onWorktreesMutated: () {
            final project = _project;
            if (project != null) {
              unawaited(_loadWorktrees(project, projectChanged: false));
            }
          },
          // B3 接缝契约：Git 页拿不到 FileWorkspaceController，dirty 确认由壳层注入。
          confirmLeaveDirty: (worktreeId) => _confirmLeaveDirty(worktreeId),
          // B4 接缝契约：hook 修复返回的 terminalSessionId 聚焦到终端面板。
          onFocusRepairSession: _focusRepairSession,
        );
      case WorkbenchPanel.worktrees:
        return WorktreesPage(
          book: widget.book,
          http: widget.http,
          project: _project!,
          activeId: _worktreeId,
          onSelect: (tree) {
            unawaited(_selectWorktree(tree, goTerminal: true));
          },
        );
      case WorkbenchPanel.automation:
        return AutomationPage(
          book: widget.book,
          http: widget.http,
          project: _project!,
          focusTaskId: _attentionFocusTaskId,
          focusOutboxId: _attentionFocusOutboxId,
          onFocusSession: (String? worktreeId, String? sessionId) {
            unawaited(_focusAutomationSession(worktreeId, sessionId));
          },
          onFocusMissing: () {
            unawaited(_focusMissingAttentionItem());
          },
          onExternalMutation: () {
            unawaited(_refreshAttentionUnread());
          },
        );
      case WorkbenchPanel.browser:
        return BrowserPage(
          key: ValueKey('browser-${_project!.id}-$_worktreeId'),
          book: widget.book,
          http: widget.http,
          project: _project!,
          worktreeId: _worktreeId,
        );
    }
  }

  /// Business Logic: Automation「打开执行现场」要把任务绑定的 worktree/session 切到终端面板
  /// （接缝契约：B8 放宽后 onFocusSession 允许单边为空，必须容忍）。
  /// Code Logic: dirty guard 不过则放弃；有 worktreeId 才 resume；有 sessionId 才切换；
  /// 双空直接忽略，不 crash、不丢当前上下文。
  Future<void> _focusAutomationSession(String? worktreeId, String? sessionId) async {
    final project = _project;
    if (project == null) {
      return;
    }
    if (worktreeId == null && sessionId == null) {
      return;
    }
    if (worktreeId != null && !await _confirmLeaveDirty(worktreeId)) {
      return;
    }
    if (!mounted) {
      return;
    }
    setState(() {
      _panel = WorkbenchPanel.terminal;
      if (sessionId != null) {
        _sessionId = sessionId;
      }
    });
    unawaited(_loadWorktrees(
      project,
      projectChanged: false,
      resumeWorktreeId: worktreeId,
    ));
  }

  /// Business Logic: Git 页 hook AI 修复会在 owning device 新建绑定 worktree 的终端，
  /// 必须切到终端面板并聚焦该 session 才能看到 agent（对齐 web handleFocusRepairSession）。
  /// Code Logic: 带 sessionId 打开终端面板（TerminalPage boot 自行刷新 sessions 并按
  /// preferredSessionId 回落选择）；worktree 保持当前 _worktreeId（修复绑定当前 worktree）；
  /// SnackBar 提示已发起修复。
  void _focusRepairSession(String sessionId) {
    final project = _project;
    if (project == null) {
      return;
    }
    _openProject(project, panel: WorkbenchPanel.terminal, sessionId: sessionId);
    _showStripSnack('已发起 hook AI 修复');
  }

  /// Business Logic: 用户在项目列表移除当前打开的项目时，必须清空项目上下文回到项目列表
  /// （对齐 web MobileWorkbench：移除激活项目后不再停留在悬空项目面板；固定接缝契约）。
  /// Code Logic: 仅当移除的是当前项目才清 _project/_sessionId/_worktreeId 并切回 projects；
  /// 其他项目被移除不影响当前上下文。
  void _onProjectRemoved(String projectId) {
    if (_project?.id != projectId) {
      return;
    }
    setState(() {
      _panel = WorkbenchPanel.projects;
      _project = null;
      _sessionId = null;
      _worktreeId = clearWorktreeOnLeaveProject();
      _worktrees = [];
    });
  }

  /// Business Logic: 聚焦的任务/outbox 已解决或已变化时，要回到「待处理」并提示，
  /// 同时刷新未读徽章保持口径一致（接缝契约）。
  /// Code Logic: 切回 attention 面板 + SnackBar 提示 + 拉一次未读数。
  Future<void> _focusMissingAttentionItem() async {
    setState(() => _panel = WorkbenchPanel.attention);
    if (mounted) {
      ScaffoldMessenger.of(context).showSnackBar(
        const SnackBar(content: Text('该事项已解决或已变化')),
      );
    }
    await _refreshAttentionUnread();
  }

  @override
  Widget build(BuildContext context) {
    final label = _server.name.isNotEmpty
        ? _server.name
        : (_server.deviceName ?? _server.baseUrl);
    return PopScope(
      // 离开工作台回地址簿时记录工作位置，供下次进入恢复。
      onPopInvokedWithResult: (didPop, result) {
        if (didPop) {
          unawaited(_saveLastLocation());
        }
      },
      child: WorkbenchShell(
        mode: _mode,
        panel: _panel,
        projectLabel: _project?.name ?? label,
        subtitle: _server.baseUrl,
        badges: {WorkbenchPanel.attention: _attentionUnread},
        automationEnabled: _automationEnabled,
        browserEnabled: _browserEnabled,
        hideWorktreeStrip: _terminalFullscreen,
        onSelect: _select,
        onBackToProjects: () {
          setState(() {
            _panel = WorkbenchPanel.projects;
            _project = null;
            _sessionId = null;
            _worktreeId = clearWorktreeOnLeaveProject();
            _worktrees = [];
            _stripMutation.reset();
            _stripMutationError = null;
          });
        },
        worktreeStrip: _project != null && shouldShowWorktreeStrip(_panel)
            ? WorktreeStrip(
                worktrees: _worktrees,
                activeId: _worktreeId,
                onSelect: (tree) {
                  unawaited(_selectWorktree(tree));
                },
                onRemove: _removeTreeFromStrip,
                busy: _removingTree || _creatingTree || _stripMutation.actionLocked,
                onCreate: _createTreeFromStrip,
                creating: _creatingTree,
                mutationError: _stripMutation.phase == GitMutationPhase.unknown
                    ? (_stripMutationError ?? '操作结果未知，请重新对账。')
                    : null,
                onRetryReconcile: _retryStripReconcile,
              )
            : null,
        child: _body(),
      ),
    );
  }
}
