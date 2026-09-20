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

/// 当前项目 worktrees 详情的加载状态（对齐 web projectDetailStatus：ready 才允许
/// 同项目早退，error 时必须给出重试入口）。
enum _ProjectDetailStatus { idle, loading, ready, error }

/// Dual-mode workbench: global 项目/待处理/传输/设置/Provider,
/// project 终端/浏览器/文件/Git/worktrees/自动化.
class WorkbenchHome extends StatefulWidget {
  const WorkbenchHome({
    super.key,
    required this.book,
    required this.http,
    @visibleForTesting this.filesWorkspace,
  });

  final AddressBook book;
  final LanHttpClient http;

  /// 测试注入：覆盖默认 Files 草稿控制器（壳层 dirty 预检/清快照共用同一实例）。
  final FileWorkspaceController? filesWorkspace;

  @override
  State<WorkbenchHome> createState() => _WorkbenchHomeState();
}

class _WorkbenchHomeState extends State<WorkbenchHome>
    with WidgetsBindingObserver {
  WorkbenchPanel _panel = WorkbenchPanel.projects;

  /// 首次激活后常驻挂载的面板集合（对齐 web /mobile：仅 files/transfer/terminal
  /// 首次激活后 hidden 常驻，保 Files 草稿上下文、终端 xterm 实例与输入流、传输进度
  /// 不因切面板销毁重建；其余面板对齐 web「切走即卸载、进入即重挂重拉」语义）。
  static const Set<WorkbenchPanel> _persistedPanels = {
    WorkbenchPanel.files,
    WorkbenchPanel.transfer,
    WorkbenchPanel.terminal,
  };

  /// 面板首访集合：常驻面板据此保持 Offstage 挂载；非常驻面板仅作导航记录，
  /// 由门控关闭/项目切换时统一清理。LinkedHashSet 保持插入序。
  final Set<WorkbenchPanel> _visitedPanels = {WorkbenchPanel.projects};

  /// 离开终端面板时是否正处于全屏（回到终端时恢复，避免 strip 在其它面板被误隐藏）。
  bool _terminalFullscreenSaved = false;
  ProjectSummary? _project;
  String? _worktreeId;
  String? _sessionId;
  List<Map<String, dynamic>> _worktrees = [];

  /// 终端页上报的「当前真实激活会话」权威 DTO（终端页内部切会话不经过壳层 setState
  /// 的 `_sessionId`）：离开终端面板/退出工作台时据此对旧会话补发 unfocus（对齐 web
  /// activeSession effect cleanup 的 compare-and-clear）；[SessionSummary.displayName]
  /// 驱动状态行会话药丸（对齐 web session={activeSession?.name}，与 chip 条同源）。
  SessionSummary? _liveTerminalSession;

  /// needsInput「看见即已读」的聚焦 epoch（面板==terminal 且激活会话 id）：
  /// 变化时清空尝试集合，同会话只标一次（对齐 web useMarkNeedsInputAttentionOnSessionFocus）。
  String _needsInputReadEpoch = '';

  /// 本 epoch 内已尝试 markRead 的 Inbox 条目 id；失败时移除以便下次触发重试。
  final Set<String> _needsInputAttemptedIds = <String>{};

  /// worktrees 列表请求代数：快速连点两个 worktree chip 时两个 list 请求并发，
  /// 旧响应后到不得把 _worktrees/_worktreeId 覆盖回先点的树（对齐 web
  /// refreshWorktrees 的 worktreesRequestIdRef 丢弃守卫）。
  int _worktreesLoadSeq = 0;

  /// 当前项目 worktrees 详情加载状态与失败文案（对齐 web projectDetailStatus/error）：
  /// 同项目早退要求 ready；error 时项目列表上方显示错误条 + 重试入口。
  _ProjectDetailStatus _projectDetailStatus = _ProjectDetailStatus.idle;
  String? _projectDetailError;

  /// 终端会话刷新令牌：Git 页/worktrees 页/终端页 worktree 变更成功后 bump，
  /// TerminalPage didUpdateWidget 检测变化后拉权威会话并清理已消失会话的常驻缓冲
  /// （对齐 web onRefreshSessions + removeBuffer 的收敛语义）。
  int _terminalSessionsToken = 0;

  /// Files 草稿控制器（测试可注入）：dirty 预检、清快照与 FilesPage 共用同一实例。
  late final FileWorkspaceController _files =
      widget.filesWorkspace ?? FileWorkspaceController();
  late final GitClient _gitClient =
      GitClient(widget.http, widget.book.active!.baseUrl);

  /// 壳层共享的会话客户端：终端 unfocus 与创建 worktree 自动开窗共用同一实例。
  late final SessionsClient _sessionsClient =
      SessionsClient(widget.http, widget.book.active!.baseUrl);

  /// 壳层共享的 Attention 客户端：徽章轮询与 needsInput 自动已读共用同一实例。
  late final AttentionClient _attentionClient =
      AttentionClient(widget.http, widget.book.active!.baseUrl);

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

  /// 终端/Git 页合并在途的壳层互斥计数与收敛布尔（对齐 web
  /// beginWorktreeOperation 全局计数锁 + worktreeOperationBusy）：
  /// 各来源 true/false 成对上报，计数 > 0 即置忙——合并（将删源树）在途期间
  /// 壳层拒绝 worktree 切换，worktrees 页卡片一并禁用；重复 true 由计数幂等收敛。
  int _externalWorktreeOpCount = 0;
  bool _externalWorktreeOpBusy = false;

  /// strip 创建 worktree 进行中（防重复提交）。
  bool _creatingTree = false;

  /// strip 删除 mutation 相位机：unknown 后锁定删除并要求同 id 对账（对齐 web bar controller）。
  final GitMutationTracker _stripMutation = GitMutationTracker();

  /// strip mutation 结果未知等错误文案（条上错误条展示，unknown 相位带「重新对账」）。
  String? _stripMutationError;

  /// attention 徽章轮询 Timer（仅 resumed 且不在 attention 面板时运行）。
  Timer? _attentionPollTimer;

  /// 当前生命周期快照；非 resumed 暂停徽章轮询（对齐 web visibilitychange 语义）。
  AppLifecycleState _lifecycleState = AppLifecycleState.resumed;

  /// lastLocation 防抖保存 Timer：panel/project/worktree/session 变化即时调度，
  /// 500ms 内合并成一次写盘（对齐 web 每次变化 replaceState 的即时持久化语义）；
  /// pop / 退后台时强制 flush，保证进程被杀后也能恢复。
  Timer? _lastLocationSaveTimer;

  /// 壳层连接态：由「最近一次 worktrees/projects 请求成败」驱动；
  /// null = 尚无成败记录（状态行不显示连接药丸，对齐 web 初始 null）。
  WorkbenchConnectionState? _connection;

  /// experimentalFeatures 拉取失败标记；Drawer 打开时据此静默重试一次。
  bool _featuresLoadFailed = false;

  ServerRecord get _server => widget.book.active!;

  WorkbenchNavMode get _mode => resolveNavMode(_panel, _project != null);

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    unawaited(_refreshAttentionUnread());
    unawaited(_restoreLastLocation());
    unawaited(_loadExperimentalFeatures());
    _syncAttentionPoll();
  }

  @override
  void dispose() {
    _attentionPollTimer?.cancel();
    _attentionPollTimer = null;
    _lastLocationSaveTimer?.cancel();
    _lastLocationSaveTimer = null;
    WidgetsBinding.instance.removeObserver(this);
    super.dispose();
  }

  /// Business Logic: attention 徽章轮询只在应用前台且有意义的面板上运行：
  /// 退后台继续轮询浪费电量与局域网请求；停在「待处理」面板时页面自身的
  /// VisibilityPoller 会回写徽章，壳层再轮询就是双请求。
  /// Code Logic: 依据 _lifecycleState 与 _panel 决定启停 10s 周期 Timer；
  /// 每次 tick 复用 _refreshAttentionUnread 静默拉取。
  void _syncAttentionPoll() {
    _attentionPollTimer?.cancel();
    _attentionPollTimer = null;
    if (_lifecycleState != AppLifecycleState.resumed ||
        _panel == WorkbenchPanel.attention) {
      return;
    }
    _attentionPollTimer = Timer.periodic(const Duration(seconds: 10), (_) {
      unawaited(_refreshAttentionUnread());
    });
  }

  /// Business Logic: 手机退后台后半开连接与轮询都应停下，回前台立即恢复，
  /// 让「待处理」未读徽章与网页版保持同粒度的新鲜度；进程被杀前要把工作位置
  /// 落盘（对齐 web 回前台 focus 立即强刷 + 位置即时持久化）。
  /// Code Logic: resumed 边沿（此前非 resumed）先 unawaited 强刷一次未读徽章
  /// （停在待处理面板时维持暂停逻辑，页面自身轮询负责回写）；paused 时强制
  /// flush lastLocation；最后统一重同步轮询 Timer。
  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    final wasResumed = _lifecycleState == AppLifecycleState.resumed;
    _lifecycleState = state;
    if (state == AppLifecycleState.resumed && !wasResumed) {
      if (_panel != WorkbenchPanel.attention) {
        unawaited(_refreshAttentionUnread());
      }
    }
    if (state == AppLifecycleState.paused) {
      unawaited(_flushSaveLastLocation());
    }
    _syncAttentionPoll();
  }

  /// Business Logic: experimentalFeatures 只在 initState 拉取一次，失败后
  /// automation/browser 入口会一直隐藏到重进工作台；用户打开 Drawer 说明正要导航，
  /// 此时静默重试一次，成功后 setState 恢复入口（fail-closed 兜底不变）。
  /// Code Logic: 仅在上次拉取失败时重试；成功/失败状态更新都由
  /// _loadExperimentalFeatures 内部 setState 完成。
  void _handleDrawerOpened() {
    if (_featuresLoadFailed) {
      unawaited(_loadExperimentalFeatures());
    }
  }

  /// Business Logic: automation/browser 是内测开关，进入工作台时要读同一份权威开关
  /// （对齐 web ExperimentalFeaturesProvider 读 GET /api/orchestrator/config 的
  /// experimentalFeatures；缺字段/失败 fail-closed 全关）。
  /// Code Logic: 宽容解析 automation/browser 两个布尔；成功后把被关闭面板的常驻
  /// 记录一并移除（卸载隐藏页面）并回落当前面板；失败记录标记等 Drawer 打开时重试。
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
        _featuresLoadFailed = false;
        if (!_automationEnabled) {
          _visitedPanels.remove(WorkbenchPanel.automation);
        }
        if (!_browserEnabled) {
          _visitedPanels.remove(WorkbenchPanel.browser);
        }
      });
      _collapseClosedPanel();
    } catch (_) {
      // fail-closed：保持全关；记录失败标记，等 Drawer 打开时静默重试。
      if (!mounted) {
        return;
      }
      setState(() => _featuresLoadFailed = true);
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
      setState(() => _gotoPanel(resolved));
    }
  }

  /// Business Logic: 切面板不再整页换 widget——首次激活的面板进入常驻集合，
  /// 隐藏面板保留 State（Files 草稿、终端 xterm/会话流、传输进度不被销毁），
  /// 对齐 web /mobile files/transfer/terminal 首次激活后 hidden 常驻的策略。
  /// Code Logic: 记录 visited + 处理终端全屏标志的暂存/恢复 + 同步徽章轮询 +
  /// 防抖持久化 lastLocation（panel 也是工作位置的一部分）；
  /// 目标与当前相同则只补 visited，不触发多余副作用。必须在 setState 内调用。
  void _gotoPanel(WorkbenchPanel next) {
    _visitedPanels.add(next);
    if (_panel == next) {
      return;
    }
    if (_panel == WorkbenchPanel.terminal) {
      // 离开终端面板：对当前会话补发 unfocus，停止旧远端窗口正文流
      // （终端面板 Offstage 常驻不会 dispose，必须显式补发，对齐 web cleanup）。
      _unfocusLiveTerminalSession();
      // 离开终端时暂存全屏状态并复位壳层，避免 strip 在其它面板被误隐藏。
      _terminalFullscreenSaved = _terminalFullscreen;
      _terminalFullscreen = false;
    } else if (next == WorkbenchPanel.terminal && _terminalFullscreenSaved) {
      _terminalFullscreenSaved = false;
      _terminalFullscreen = true;
    }
    _panel = next;
    _syncAttentionPoll();
    _scheduleSaveLastLocation();
  }

  /// fire-and-forget unfocus 当前终端会话：停止其远端窗口正文流过滤目标
  /// （失败静默，下一次 focus 以当前窗口重建；对齐 web sessions.focus(id, false)）。
  void _unfocusLiveTerminalSession() {
    final id = _liveTerminalSession?.id;
    if (id == null || id.isEmpty) {
      return;
    }
    unawaited(_sessionsClient.focus(id, streamActive: false).catchError((_) {}));
  }

  /// Business Logic: 进入工作台时 Drawer「待处理」要显示未读数，且数字必须与列表一致。
  /// Code Logic: 拉取移动端可见条目，按 filter.dart 的本地日口径统计今天未读；离线失败静默保留旧值。
  /// 刷新成功后用同一份快照重查「聚焦会话的 needsInput 未读」（对齐 web 快照驱动
  /// 的看见即已读：停留在终端期间新到达的条目也立即标已读）。
  Future<void> _refreshAttentionUnread() async {
    List<AttentionItem> items;
    try {
      items = await _attentionClient.listVisible();
    } catch (_) {
      return;
    }
    if (!mounted) {
      return;
    }
    setState(() {
      _attentionUnread = countTodayUnreadAttentionItems(items, DateTime.now());
    });
    final activeSession = _panel == WorkbenchPanel.terminal
        ? (_liveTerminalSession?.id ?? _sessionId)
        : null;
    if (activeSession != null) {
      await _markNeedsInputReadFromSnapshot(activeSession, items);
    }
  }

  /// Business Logic: 用户切到正在等待输入的终端即表示已经看见，对应 Inbox 未读
  /// 条目应立即收起、徽章下降，不必回「待处理」逐条点开（对齐 web
  /// useMarkNeedsInputAttentionOnSessionFocus 的「看见即已读」）。
  ///
  /// Code Logic: build 时比较聚焦 epoch（面板==terminal 且激活会话 id）——变化则
  /// 清空尝试集合并帧末异步执行标记；命中「未读 + sourceKind=agentNeedsInput +
  /// target.agentSession.terminalSessionId==当前会话」的条目后 fire-and-forget
  /// markRead，成功刷新徽章，失败回退尝试集合并保持未读（下次触发可重试）。
  void _scheduleNeedsInputAutoRead() {
    final activeSession = _panel == WorkbenchPanel.terminal
        ? (_liveTerminalSession?.id ?? _sessionId)
        : null;
    final epoch = activeSession ?? '';
    if (epoch == _needsInputReadEpoch) {
      return;
    }
    _needsInputReadEpoch = epoch;
    _needsInputAttemptedIds.clear();
    if (activeSession == null) {
      return;
    }
    final sessionId = activeSession;
    WidgetsBinding.instance.addPostFrameCallback((_) {
      unawaited(_markNeedsInputReadFor(sessionId));
    });
  }

  /// 拉取 Inbox 并把当前会话对应的未读 needsInput 条目标已读（见
  /// [_scheduleNeedsInputAutoRead]）；任何失败都静默，不阻塞终端交互。
  Future<void> _markNeedsInputReadFor(String sessionId) async {
    List<AttentionItem> items;
    try {
      items = await _attentionClient.listVisible();
    } catch (_) {
      return;
    }
    if (!mounted) {
      return;
    }
    await _markNeedsInputReadFromSnapshot(sessionId, items);
  }

  /// Business Logic: 用户停留在同一终端会话期间 Agent 新发起等待输入时，快照里
  /// 新出现的未读条目也要立即标已读（对齐 web 该 hook 依赖 snapshot、每次快照
  /// 变化都重新 plan 的分支），不能等切会话/切面板再回来才补标。
  ///
  /// Code Logic: 接收一次已拉取的 Inbox 快照，按当前会话匹配未读 needsInput
  /// 条目并 fire-and-forget markRead；成功刷新徽章（其自身的快照重查因条目已读
  /// 而空跑收敛），失败回退尝试集合保持未读，由下一次快照重查重试。
  Future<void> _markNeedsInputReadFromSnapshot(
    String sessionId,
    List<AttentionItem> items,
  ) async {
    final ids = <String>[
      for (final item in items)
        if (item.isUnread &&
            item.sourceKind == 'agentNeedsInput' &&
            item.targetKind == 'agentSession' &&
            item.sessionId == sessionId)
          item.id,
    ].where((id) => !_needsInputAttemptedIds.contains(id)).toList();
    if (ids.isEmpty) {
      return;
    }
    for (final id in ids) {
      _needsInputAttemptedIds.add(id);
    }
    try {
      await _attentionClient.markRead(ids);
      await _refreshAttentionUnread();
    } catch (_) {
      // 失败保持未读：回退尝试集合，下次快照重查可重试（对齐 web）。
      _needsInputAttemptedIds.removeAll(ids);
    }
  }

  /// Business Logic: 离开工作台回地址簿后，再次进入同一 PC 应恢复上次的项目/面板/worktree/session；
  /// panel/project/worktree/session 任一变化时也要即时持久化（对齐 web 每次变化
  /// replaceState 的语义），进程被杀后才能恢复到最后位置。
  /// Code Logic: 弹出路由/paused 时把当前工作位置写入该 server 的 lastLocation 并持久化；
  /// 未打开项目则不覆盖。
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

  /// Business Logic: panel/project/worktree/session 高频连续变化时不必每次都写盘，
  /// 但也不能丢更新——500ms 防抖合并，最后状态必然被写入。
  /// Code Logic: 重置防抖 Timer，到期后异步执行 _saveLastLocation。
  void _scheduleSaveLastLocation() {
    _lastLocationSaveTimer?.cancel();
    _lastLocationSaveTimer = Timer(const Duration(milliseconds: 500), () {
      unawaited(_saveLastLocation());
    });
  }

  /// Business Logic: pop / 退后台这类「可能不再有机会写盘」的时点必须绕过防抖
  /// 立即落盘（对齐 web 进程被杀后可恢复的要求）。
  /// Code Logic: 取消挂起的 Timer 并同步保存当前快照。
  Future<void> _flushSaveLastLocation() async {
    _lastLocationSaveTimer?.cancel();
    _lastLocationSaveTimer = null;
    await _saveLastLocation();
  }

  /// Business Logic: 再次进入该 PC 工作台时应尽量回到上次的工作位置；
  /// 这里的 projects 请求成败同样驱动壳层连接态（对齐 web「最近一次
  /// worktrees/projects 请求成败」口径）。
  /// Code Logic: 取最近项目列表后用纯函数回落解析（项目不在列表/无记录则保持现状）；
  /// worktree 用 resumeWorktreeId 在新鲜列表里校验（无效回落主树）；session 由
  /// TerminalPage 自行回落。
  Future<void> _restoreLastLocation() async {
    final location = _server.lastLocation;
    try {
      final projects = await ProjectsClient(widget.http, _server.baseUrl).listRecent();
      if (!mounted) {
        return;
      }
      _noteConnectionSuccess();
      final restore = resolveWorkbenchLocationRestore(
        location: location,
        recentProjectIds: projects.map((p) => p.id).toSet(),
      );
      if (restore == null) {
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
    } catch (error) {
      if (!mounted) {
        return;
      }
      _noteConnectionFailure(error);
    }
  }

  /// Business Logic: 终端/文件/Git 与 worktrees 页都要看运行期 Git 状态
  /// （状态点/badge/可推送），列表统一带 includeGitStatus 拉取。请求成败同时
  /// 驱动壳层连接态：失败进入离线态（保留缓存提示），失败后的下一次成功视为
  /// 恢复边沿并对当前项目重跑拉取（对齐 web 断线恢复自动刷新权威数据）。
  /// 本次加载也是当前项目的「worktrees 详情」：成败同步推进详情状态
  /// （ready/error，对齐 web projectDetailStatus），供同项目早退门槛与项目列表
  /// 错误条重试入口使用。
  ///
  /// Code Logic: 进入即自增 _worktreesLoadSeq 并捕获局部快照；每次 await 之后、
  /// 写 _worktrees/_worktreeId（setState）之前校验自己仍是最新一次请求且页面仍在，
  /// 否则直接丢弃——快速连点两个 worktree chip 时旧响应晚到不得覆盖新选中。
  /// 成功/失败分别上报连接态与详情状态（丢弃的旧响应不上报）。
  Future<void> _loadWorktrees(
    ProjectSummary project, {
    required bool projectChanged,
    String? resumeWorktreeId,
  }) async {
    _worktreesLoadSeq += 1;
    final seq = _worktreesLoadSeq;
    try {
      final body = await _gitClient.listWorktrees(project.id, includeGitStatus: true);
      if (seq != _worktreesLoadSeq || !mounted) {
        return;
      }
      _noteConnectionSuccess();
      final trees = asObjectList(body, wrapKey: 'worktrees');
      setState(() {
        _worktrees = trees;
        _projectDetailStatus = _ProjectDetailStatus.ready;
        _projectDetailError = null;
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
      _scheduleSaveLastLocation();
    } catch (error) {
      if (seq != _worktreesLoadSeq || !mounted) {
        return;
      }
      _noteConnectionFailure(error);
      final message = error.toString();
      setState(() {
        _projectDetailStatus = _ProjectDetailStatus.error;
        _projectDetailError =
            message.length > 160 ? '${message.substring(0, 160)}…' : message;
      });
    }
  }

  /// Business Logic: 壳层用「最近一次 worktrees/projects 请求成败」维护简易连接态；
  /// 从离线恢复在线的边沿要对当前项目重拉权威 worktrees（对齐 web
  /// shouldRefreshMobilePanelOnReconnect 的自动刷新；终端会话重连由终端页自理）。
  /// Code Logic: 记录 online（含成功时间）；prev 非空且非 online 时视为恢复边沿，
  /// 触发一次当前项目的 _loadWorktrees（该请求成功后 prev 已是 online，不会递归）。
  void _noteConnectionSuccess() {
    final prev = _connection;
    final next = WorkbenchConnectionState.online(lastSucceededAt: DateTime.now());
    final recovered = shouldRefreshWorkbenchOnReconnect(prev, next);
    setState(() => _connection = next);
    if (recovered) {
      final project = _project;
      if (project != null) {
        unawaited(_loadWorktrees(project, projectChanged: false));
      }
    }
  }

  /// Business Logic: 请求失败后状态行要显示「离线 + 最近错误」与「缓存于」提示，
  /// 让用户知道当前数据来自缓存（对齐 web markMobileConnectionOffline）。
  /// Code Logic: 保留上次成功时间做 cachedSince；错误文案截断避免刷屏。
  void _noteConnectionFailure(Object error) {
    final message = error.toString();
    setState(() {
      _connection = markWorkbenchConnectionFailure(
        message.length > 160 ? '${message.substring(0, 160)}…' : message,
        _connection,
      );
    });
  }

  /// Business Logic: 删除的若是当前打开的项目，未保存的文件草稿会随上下文一起消失，
  /// 需要先确认（对齐 web 移除激活项目前先过 Files dirty guard）。
  /// Code Logic: 非激活项目直接放行；激活项目复用 _confirmLeaveDirty 的确认对话框，
  /// 用户选择丢弃时清 dirty 快照后放行，取消则中止删除。
  Future<bool> _confirmProjectRemove(ProjectSummary project) async {
    if (project.id != _project?.id) {
      return true;
    }
    return _confirmLeaveDirty(_worktreeId ?? '');
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

  /// Business Logic: 切换到不同项目前，Files 未保存草稿会随旧项目上下文一起失效，
  /// 必须先让用户显式处置（对齐 web confirmFileContextSwitch 的跨项目预检；三选
  /// 「取消/丢弃/保存」对齐 AGENTS.md 文档与文件预览页 PopScope 的同款三选）。
  ///
  /// Code Logic: 无 dirty 快照直接放行；有则弹三选——取消中止切换（不调后端、
  /// 不清快照）；丢弃清 dirty 快照后放行；保存经草稿页注册的保存委托执行真实保存，
  /// 成功（内部已 markClean）放行、失败保持 dirty 并中止切换。草稿页未注册委托
  /// （canSave=false）时只提供取消/丢弃两项。
  Future<bool> _confirmProjectSwitchLeaveDirty() async {
    if (!_files.snapshot.dirty) {
      return true;
    }
    final choice = await showDialog<String>(
      context: context,
      builder: (context) => AlertDialog(
        key: const Key('project-switch-dirty-dialog'),
        title: const Text('未保存的文件'),
        content: const Text('切换项目前请保存或丢弃当前文件草稿。'),
        actions: [
          TextButton(
            key: const Key('project-switch-dirty-cancel'),
            onPressed: () => Navigator.pop(context, 'cancel'),
            child: const Text('取消'),
          ),
          TextButton(
            key: const Key('project-switch-dirty-discard'),
            onPressed: () => Navigator.pop(context, 'discard'),
            child: const Text('丢弃'),
          ),
          if (_files.canSave)
            FilledButton(
              key: const Key('project-switch-dirty-save'),
              onPressed: () => Navigator.pop(context, 'save'),
              child: const Text('保存'),
            ),
        ],
      ),
    );
    if (!mounted) {
      return false;
    }
    if (choice == 'discard') {
      _files.markClean();
      return true;
    }
    if (choice == 'save') {
      return _files.save();
    }
    return false;
  }

  /// Business Logic: 终端 FAB 一键合并激活 worktree 会删除源树，Files 未保存草稿
  /// 可能随之变成孤儿——合并确认框前先做只读预检（对齐 web
  /// runMobileWorktreeMergeFlow.confirmActiveWorktreeChange），取消不调后端。
  ///
  /// Code Logic: 仅合并源==当前激活树时预检（TerminalPage 恒绑定激活树，此判断
  /// 兜底异常接线）；主工作区 collect-merge 不删源不切上下文，直接放行（对齐 web
  /// getMobileWorktreeMergePlan 的 requiresActivePreflight=false）；功能树按「合并后
  /// 回落树」（除源外主树优先）比较 dirty 上下文，命中复用 [_confirmLeaveDirty]
  /// 确认——选择丢弃即清 dirty 快照，合并成功回落主树后不会对已删 worktree 再弹
  /// 「请保存或丢弃」。
  Future<bool> _confirmTerminalMergeLeaveDirty(String worktreeId) async {
    if (worktreeId.isEmpty || worktreeId != _worktreeId) {
      return true;
    }
    final source = _currentWorktreeInfo;
    if (source != null && source['isMain'] == true) {
      return true;
    }
    final remaining = [
      for (final tree in _worktrees)
        if (tree['id'] != worktreeId) tree,
    ];
    final fallbackId = resolveActiveWorktreeId(
      trees: remaining,
      previousId: null,
      projectChanged: true,
    );
    if (fallbackId == null || fallbackId == worktreeId) {
      return true;
    }
    return _confirmLeaveDirty(fallbackId);
  }

  /// Business Logic: 终端/Git 页合并上报的 worktree 操作互斥回调：true 计数 +1、
  /// false 计数 -1（clamp 0 防御性收敛），> 0 即置忙。多个来源（终端 Offstage
  /// 常驻 + Git 页）并发时按计数收敛；setState 仅在忙布尔翻转时触发。
  /// Code Logic: 计数收敛 + 布尔缓存驱动 [_selectWorktree] 守卫与 worktrees 页
  /// `externalBusy`（对齐 web worktreeOperationBusyRef + setWorktreeOperationBusy）。
  void _handleWorktreeOperationBusyChanged(bool busy) {
    _externalWorktreeOpCount = busy
        ? _externalWorktreeOpCount + 1
        : (_externalWorktreeOpCount - 1).clamp(0, 1 << 30);
    final next = _externalWorktreeOpCount > 0;
    if (next == _externalWorktreeOpBusy) {
      return;
    }
    _externalWorktreeOpBusy = next;
    if (mounted) {
      setState(() {});
    }
  }

  /// Business Logic: worktrees 页/切换条选中新 worktree 后的统一入口：
  /// dirty 确认 → 写入 worktreeId；goTerminal 决定是否自动进入终端面板
  /// （对齐 web：点击 worktree 卡片切换后自动进入终端）。strip 删除/创建等
  /// worktree 操作全程在途时拒绝切换（对齐 web beginWorktreeOperation 的
  /// worktreeOperationBusy 互斥——创建全程持锁，不只是删除），避免与在途
  /// 刷新/删除/创建竞态；终端/Git 页合并在途（壳层互斥计数 > 0）同样拒绝
  /// （合并会删除源 worktree，切走激活树会与删除回落竞态，仅靠请求序号兜底）。
  /// Code Logic: 创建在途、strip mutation 非 idle、删除在途或外部合并置忙则
  /// 提示并放弃；dirty guard 不过则放弃；同树只更新选中，跨树刷新列表并回落 active。
  Future<void> _selectWorktree(
    Map<String, dynamic> tree, {
    bool goTerminal = false,
  }) async {
    final id = tree['id'] as String? ?? '';
    if (id.isEmpty) {
      return;
    }
    if (_removingTree ||
        _creatingTree ||
        _stripMutation.actionLocked ||
        _externalWorktreeOpBusy) {
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(
          const SnackBar(content: Text('正在处理 worktree 操作，请稍候')),
        );
      }
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
        _gotoPanel(WorkbenchPanel.terminal);
      }
    });
    _scheduleSaveLastLocation();
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
    // 对齐 web runMobileWorktreeRemovalFlow：删除激活 worktree 前先做只读脏文件预检，
    // 取消则不调后端；选择丢弃会清 dirty 快照，未保存草稿不再随删除静默丢失。
    if (id == _worktreeId && !await _confirmLeaveDirty(id)) {
      return;
    }
    if (!mounted) {
      return;
    }
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('移除 worktree'),
        content: Text(worktreeStripRemoveConfirmText(name)),
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
      sessions: _sessionsClient,
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

  /// 终端页上报当前真实激活会话：写字段 + 帧末刷新状态行并重估「看见即已读」epoch
  /// （setState 推迟到帧末且先校验 mounted——终端页 dispose/重建可能发生在壳层
  /// build/卸载期间，不能在上报路径上同步 setState）。
  ///
  /// Code Logic：字段驱动三件事——fire-and-forget unfocus（离开面板/退出工作台）、
  /// 状态行会话药丸（displayName，对齐 web session={activeSession?.name}）、
  /// needsInput「看见即已读」epoch 比较。
  void _handleLiveSessionChanged(SessionSummary? session) {
    if (_liveTerminalSession?.id == session?.id &&
        (_liveTerminalSession != null) == (session != null)) {
      return;
    }
    _liveTerminalSession = session;
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!mounted) {
        return;
      }
      setState(() {});
      _scheduleNeedsInputAutoRead();
    });
  }

  /// Business Logic: 壳层状态行的 worktree 药丸要显示权威列表里的显示名
  /// （name/branch 兜底口径与切换条一致）；列表尚未拉到时回落 worktreeId，
  /// 二者皆缺则交由壳层渲染「worktree」占位（对齐 web status.worktree）。
  /// Code Logic: 纯读——从 _currentWorktreeInfo 取 worktreeDisplayName。
  String? _statusWorktreeLabel() {
    final info = _currentWorktreeInfo;
    if (info != null) {
      final name = worktreeDisplayName(info);
      if (name.isNotEmpty) {
        return name;
      }
    }
    return _worktreeId;
  }

  /// Business Logic: 打开项目进入工作台。返回项目列表不再清空上下文后，项目列表
  /// 点「同一项目」要在 worktrees 详情加载成功（ready）时才直接回到原 worktree/
  /// session（不重拉、不走 dirty 确认——上下文没变，对齐 web
  /// shouldSkipMobileProjectReload 仅 ready 早退）；上次详情加载失败（error）时点
  /// 同项目即完整重拉，成为恢复入口。切换到不同项目才走完整切换流程：先过 Files
  /// dirty 三选预检（取消不调后端），通过后清旧项目上下文再拉权威列表。
  /// attention/automation 聚焦同项目的指定 session 时不受早退影响，仍要精确切换。
  ///
  /// Code Logic: 同项目且详情 ready 且未显式指定 worktree/session → 只切目标面板；
  /// 不同项目先 [_confirmProjectSwitchLeaveDirty]（取消直接返回）；否则沿用既有
  /// 流程：详情置 loading、项目变化时清 worktree/常驻面板/mutation 锁，再拉权威
  /// 列表（成败由 [_loadWorktrees] 推进详情 ready/error）。
  Future<void> _openProject(
    ProjectSummary project, {
    WorkbenchPanel panel = WorkbenchPanel.terminal,
    String? sessionId,
    String? resumeWorktreeId,
  }) async {
    final sameProject = _project?.id == project.id;
    // 恢复/跳转目标面板若被内测开关关闭，则回落到可用面板。
    final gated = resolvePanelForFeatures(
      panel: panel,
      hasProject: true,
      automationEnabled: _automationEnabled,
      browserEnabled: _browserEnabled,
    );
    if (sameProject &&
        _projectDetailStatus == _ProjectDetailStatus.ready &&
        sessionId == null &&
        resumeWorktreeId == null) {
      setState(() => _gotoPanel(gated));
      return;
    }
    final projectChanged = !sameProject;
    if (projectChanged && !await _confirmProjectSwitchLeaveDirty()) {
      return;
    }
    if (!mounted) {
      return;
    }
    setState(() {
      _project = project;
      _sessionId = sessionId;
      _projectDetailStatus = _ProjectDetailStatus.loading;
      _projectDetailError = null;
      if (projectChanged) {
        _worktreeId = clearWorktreeOnLeaveProject();
        _worktrees = [];
        // 切项目后旧 mutation unknown 锁不得污染新上下文（对齐 web 重置 effect）。
        _stripMutation.reset();
        _stripMutationError = null;
        // 旧项目的项目级面板（文件/Git/浏览器/worktrees/自动化）整体失效：
        // 清除常驻记录让其随旧项目卸载，避免隐藏页面持有旧项目上下文。
        // 终端例外：终端页跨项目 hidden 常驻（对齐 web terminal 面板常驻 +
        // backgroundSessions 跨项目保留输出缓冲），页面经 didUpdateWidget 自行
        // 断开旧上下文并按新项目重新 boot，切回原项目时命中缓冲不清屏不重放。
        _visitedPanels.removeAll(
          kProjectBoundPanels.where((panel) => panel != WorkbenchPanel.terminal),
        );
      }
      _gotoPanel(gated);
    });
    _scheduleSaveLastLocation();
    _loadWorktrees(
      project,
      projectChanged: projectChanged,
      resumeWorktreeId: resumeWorktreeId,
    );
  }

  /// Business Logic: worktrees 详情加载失败后，项目列表错误条上的「重试」要能
  /// 显式重拉当前项目详情（对齐 web handleReloadProjectDetails 的 forceReload）；
  /// 成功后错误条随之清除（详情状态由 [_loadWorktrees] 推进为 ready）。
  /// Code Logic: 仅在有激活项目且非 loading 中时响应；置 loading 清错误后重跑加载。
  void _retryProjectDetail() {
    final project = _project;
    if (project == null || _projectDetailStatus == _ProjectDetailStatus.loading) {
      return;
    }
    setState(() {
      _projectDetailStatus = _ProjectDetailStatus.loading;
      _projectDetailError = null;
    });
    unawaited(_loadWorktrees(project, projectChanged: false));
  }

  /// Business Logic: Git 页/worktrees 页/终端页的 worktree 变更（删除/合并/commit
  /// 收敛）成功后必须统一收敛壳层：重拉权威 worktrees（active 失效按主树优先回落，
  /// 对齐 web onWorktreesChange + setActiveWorktreeWithSession 兜底）并 bump 终端
  /// 会话刷新令牌（TerminalPage 据此拉权威会话并清理已消失会话的常驻缓冲，对齐 web
  /// onRefreshSessions + removeBuffer）。
  /// Code Logic: 列表重拉走既有 [_loadWorktrees]（含请求序号守卫与回落解析）；
  /// 令牌自增触发 TerminalPage didUpdateWidget。
  void _handleWorktreesMutated() {
    final project = _project;
    if (project != null) {
      unawaited(_loadWorktrees(project, projectChanged: false));
    }
    setState(() => _terminalSessionsToken += 1);
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
    setState(() => _gotoPanel(gated));
  }

  /// Business Logic: 切面板不应销毁常驻面板的 State——Files 草稿上下文、终端 xterm
  /// 实例与输入流、传输进度 UI 要求首次激活后 hidden 常驻（对齐 web /mobile 仅
  /// files/transfer/terminal 常驻的策略）；其余面板切走即卸载、进入即重挂重拉，
  /// 保证 Git/worktrees/自动化/浏览器等每次进入都是权威新鲜数据。
  /// Code Logic: 常驻面板渲染 Offstage(offstage: panel != _panel)，Stack fit: expand
  /// 保证可见面板占满剩余空间、布局与原单面板一致；Offstage 子树参与 layout 但不
  /// 绘制、不命中。每个 Offstage 带 ValueKey，保证 visited 集合中间元素被移除后其余
  /// 面板的 State 仍按 key 配对复用。非常驻面板仅在激活时作为普通子节点渲染（无
  /// Offstage 包裹，切走即销毁）。项目绑定面板在无项目上下文时跳过渲染（防 _project!
  /// 解引用崩溃）。
  Widget _panelStack() {
    return Stack(
      fit: StackFit.expand,
      children: [
        for (final panel in _visitedPanels)
          if (!isProjectBoundPanel(panel) || _project != null)
            if (_persistedPanels.contains(panel))
              Offstage(
                key: ValueKey(panel),
                offstage: panel != _panel,
                child: _buildPanelBody(panel),
              )
            else if (panel == _panel)
              _buildPanelBody(panel),
      ],
    );
  }

  /// Business Logic: 各面板的 widget 构造（props 每次构建用当前壳层状态新造，
  /// 面板 State 因 runtimeType + key 稳定而保留；TerminalPage/FilesPage 的
  /// ValueKey 语义不变——key 变化即有意重置上下文）。
  /// Code Logic: 按 panel 参数 switch 返回对应页面；项目级面板由 _panelStack
  /// 保证仅在 _project != null 时调用。
  Widget _buildPanelBody(WorkbenchPanel panel) {
    switch (panel) {
      case WorkbenchPanel.projects:
        return ProjectsPage(
          book: widget.book,
          http: widget.http,
          onOpen: (project) => unawaited(_openProject(project)),
          onProjectRemoved: _onProjectRemoved,
          confirmRemove: _confirmProjectRemove,
          activeProjectId: _project?.id,
          // worktrees 详情 error 态：项目列表上方错误条 + 重试（对齐 web
          // projectDetailRetry；仅当前项目可见）。
          detailError: _project != null &&
                  _projectDetailStatus == _ProjectDetailStatus.error
              ? (_projectDetailError ?? '加载失败')
              : null,
          onRetryDetail: _retryProjectDetail,
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
          // key 稳定：终端面板跨项目/worktree/session 常驻（对齐 web terminal 面板
          // hidden 常驻 + backgroundSessions 跨项目保留输出缓冲）。project/worktree/
          // preferredSession 变化由页面 didUpdateWidget 断开旧上下文并重新 boot，
          // _mounted 缓冲按 sessionId 跨项目保留，切回原项目不清屏不重放。
          key: ValueKey('terminal'),
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
          onWorktreesMutated: _handleWorktreesMutated,
          // Issue2 接缝契约：终端合并全程上报壳层互斥锁（拒绝切换 + 禁用 worktrees 卡片）。
          onWorktreeOperationBusyChanged: _handleWorktreeOperationBusyChanged,
          // 合并激活树前先过 Files dirty 预检（取消不调后端；丢弃清快照，
          // 成功回落主树后不对已删树弹「请保存或丢弃」，对齐 web merge flow）。
          confirmLeaveDirty: _confirmTerminalMergeLeaveDirty,
          onActiveSessionChanged: _handleLiveSessionChanged,
          sessionsRefreshToken: _terminalSessionsToken,
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
          // B3 接缝契约：merge/commit 成功后回写壳层统一收敛（重拉列表 + bump
          // 终端会话刷新令牌）。
          onWorktreesMutated: _handleWorktreesMutated,
          // Issue2 接缝契约：Git 页合并全程上报壳层互斥锁（拒绝切换 + 禁用 worktrees 卡片）。
          onWorktreeOperationBusyChanged: _handleWorktreeOperationBusyChanged,
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
          confirmLeaveDirty: (worktreeId) => _confirmLeaveDirty(worktreeId),
          // 删除/合并成功后壳层统一收敛：重拉权威列表（active 失效回落主树，
          // strip 不残留已删树）+ bump 终端会话刷新令牌（对齐 web
          // MobileWorktreePanel onWorktreesChange/onRefreshSessions 回写）。
          onWorktreesMutated: _handleWorktreesMutated,
          // Issue2 接缝契约：终端/Git 页合并在途（壳层互斥锁置忙）时卡片选择
          // 与合并/移除入口一并禁用（与页内忙碌锁同口径，对齐 web
          // MobileWorktreePanel busy → isActionDisabled）。
          externalBusy: _externalWorktreeOpBusy,
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
      _gotoPanel(WorkbenchPanel.terminal);
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
  /// 其他项目被移除不影响当前上下文。详情状态一并复位（旧项目的 error/loading
  /// 不得投射到项目列表）。
  void _onProjectRemoved(String projectId) {
    if (_project?.id != projectId) {
      return;
    }
    setState(() {
      _gotoPanel(WorkbenchPanel.projects);
      _project = null;
      _sessionId = null;
      _worktreeId = clearWorktreeOnLeaveProject();
      _worktrees = [];
      _projectDetailStatus = _ProjectDetailStatus.idle;
      _projectDetailError = null;
      // 激活项目被移除：项目级常驻面板整体失效并卸载。
      _visitedPanels.removeAll(kProjectBoundPanels);
    });
  }

  /// Business Logic: 聚焦的任务/outbox 已解决或已变化时，要回到「待处理」并提示，
  /// 同时刷新未读徽章保持口径一致（接缝契约）。
  /// Code Logic: 切回 attention 面板 + SnackBar 提示 + 拉一次未读数。
  Future<void> _focusMissingAttentionItem() async {
    setState(() => _gotoPanel(WorkbenchPanel.attention));
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
    // 「看见即已读」：面板切到终端且激活会话变化时标已读对应 needsInput 条目。
    _scheduleNeedsInputAutoRead();
    return PopScope(
      // 离开工作台回地址簿时强制落盘工作位置（绕过防抖），并对当前会话补发
      // unfocus 停止远端窗口正文流（对齐 web 离开 Mobile Workbench 的 cleanup）。
      onPopInvokedWithResult: (didPop, result) {
        if (didPop) {
          _unfocusLiveTerminalSession();
          unawaited(_flushSaveLastLocation());
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
        // 终端全屏对齐 web fixed overlay：盖住整个 shell chrome（AppBar + 状态行）。
        hideAppBar: _terminalFullscreen,
        connection: _connection,
        worktreeLabel: _statusWorktreeLabel(),
        // 状态行会话药丸显示激活会话显示名（与 chip 条同源），无激活会话回落 null
        // （壳层渲染「session」占位，对齐 web session={activeSession?.name ?? null}）。
        sessionLabel: _liveTerminalSession?.displayName,
        onSelect: _select,
        onDrawerOpened: _handleDrawerOpened,
        onBackToProjects: () {
          // 对齐 web handleBackToProjects：只切面板，不清项目/worktree/session 上下文；
          // 项目列表点同一项目直接秒回原工作位置（见 _openProject 同项目早退）。
          setState(() => _gotoPanel(WorkbenchPanel.projects));
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
        child: _panelStack(),
      ),
    );
  }
}
