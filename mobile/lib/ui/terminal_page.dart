import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:image_picker/image_picker.dart';
import 'package:xterm/xterm.dart' hide TerminalController;
import 'package:xterm/xterm.dart' as xterm show TerminalController;

import '../address_book/book.dart';
import '../core/lan_http.dart';
import '../git/client.dart';
import '../git/mutation.dart';
import '../projects/client.dart';
import '../prompts/client.dart';
import '../sessions/client.dart';
import '../terminal/controller.dart';
import '../terminal/extra_keys.dart';
import '../terminal/git_actions.dart';
import '../terminal/touch_scroll.dart';
import 'extra_keys_bar.dart';

/// 收藏面板「全部」标签哨兵，避免与真实 tag 字面量冲突（对齐 web FAVORITE_ALL_TAG）。
const String _kFavoriteAllTag = '__all__';

/// 常驻终端缓冲上限：超出后淘汰最久未用的非当前会话（对齐 web
/// mobileMountedTerminals.ts MAX_MOUNTED_MOBILE_TERMINALS = 8）。
///
/// 已挂载会话保留输出缓冲，切回不清屏、不重放，用 events 增量（afterSequence）
/// 补齐 gap；仅在序列过旧触发 gap 帧时才走全量 replay。
const int _kMaxMountedTerminalBuffers = 8;

/// 回前台后视口钉住最新输出的时长（对齐 web mobileTerminalResumeFollow.ts 8s）。
const int _kResumeFollowMs = 8000;

/// 单个会话的常驻运行时：输出缓冲（xterm Terminal）+ 协议策略 + 事件基线。
///
/// Business Logic（为什么需要）:
///   多会话切换时保留各自的画面与协议状态（web MobileTerminalXtermSlot 每会话一份
///   Terminal/Buffer），切回时不清屏不重放；后台期间无法积累增量（events 每会话一条
///   连接），靠 afterSequence 续传，序列过旧由 gap 帧兜底全量 replay。
///
/// Code Logic（做什么）:
///   持有 TerminalController（本会话 id 构造，replayReady/hydration/inputLink 均
///   会话私有）、Terminal 输出缓冲、ownerInstanceId/sequence 事件基线、resize 上报
///   基线与 hydration 触发状态；由页面的 LRU 表管理创建/复用/淘汰。
class _MountedSession {
  _MountedSession(
    this.sessionId, {
    required this.projectId,
    required void Function(String) onOutput,
  }) : policy = TerminalController(sessionId: sessionId),
       terminal = Terminal(maxLines: 5000, onOutput: onOutput);

  /// 会话 id（与 policy.sessionId 一致，供 LRU 键与日志使用）。
  final String sessionId;

  /// 该 buffer 所属项目 id：令牌驱动的 prune 只清理「当前项目内已消失会话」的
  /// 常驻缓冲，其它项目的缓冲跨项目保留不受影响（对齐 web sessionsRef 仅含
  /// 当前项目会话的口径）。
  final String projectId;

  /// 本会话的协议策略（输入链路握手/背压、replayReady、hydration 标记均会话私有）。
  final TerminalController policy;

  /// 本会话的输出缓冲；切走时保留，切回时由 TerminalView 重新挂载。
  final Terminal terminal;

  /// 最近一次 events 帧的 ownerInstanceId（ reconnect 时作 afterOwnerInstanceId）。
  String? owner;

  /// 最近一次 events 帧的 sequence（reconnect 时作 afterSequence 增量续传）。
  int sequence = 0;

  /// resize 上报基线：创建时优先取服务端持久化 session.cols/rows（同尺寸不回传，
  /// 对齐 web XtermSlot persistedSessionSize）；上报后记录实测值。
  (String, int, int)? lastSentResize;

  /// 服务端持久化 PTY 尺寸（创建该 buffer 时的会话 DTO 快照；旧后端无值为 null）。
  int? persistedCols;
  int? persistedRows;

  /// hydration（refreshHistory replay）单飞门闩。
  bool hydrating = false;

  /// hydration 触发前累计的向上滚动意图（行）。
  int hydrationIntent = 0;

  /// hydration（refreshHistory replay）在途期间暂存的实时输出 chunk：
  /// 元素为 (帧 sequence, 帧 ownerInstanceId, chunk 文本)，sequence/owner 缺失时为 null。
  ///
  /// Business Logic: hydration 往返期间 events 流仍会送达 live chunk；若照常写入
  /// terminal，会被快照返回后的「清屏 + 写快照」吞掉，且这些帧的 seq 已推进事件基线、
  /// 服务端不会重发（对齐 web hydration held live 的防丢失语义）。
  ///
  /// Code Logic: 在途标志复用 [hydrating]；events 循环写 chunk 前检查该标志，在途则
  /// 追加到此列表而不写 terminal；快照返回后由页面按序补写「seq > 快照 lastSeq 且
  /// owner 一致」的 chunk，失败/切走路径全部按序补写。
  final List<(int?, String?, String)> heldLiveChunks =
      <(int?, String?, String)>[];
}

/// 局域网远端项目终端页。
///
/// Business Logic（为什么需要）:
///   手机上需要实时查看并输入远端 Workbench 终端：输出走 NDJSON events 长连接，
///   输入走独立 WebSocket；局域网环境两者都会频繁半开断连，必须自动重连才能可靠使用。
///   同时终端页要补齐与网页版移动端同等的能力：会话关闭与多窗格（pane）操作、
///   全屏、触控滚动转发（TUI 内 SGR wheel）、惰性历史 hydration、终端内 Git
///   提交/合并与钩子 AI 修复、收藏 Prompt 快捷输入与 Prompt 优化。
///
/// Code Logic（做什么）:
///   会话切换 = 清屏 + replay 快照 + 事件基线归零 + 重建输入 WS + 重启 events 循环 + zoom-pane 幂等；
///   events 断开后固定 2s 节奏重连并携带最近 ownerInstanceId+afterSequence，
///   35 秒无任何帧主动断开重连，回前台立即重连；输入 WS 断开后 1s→…→10s 重建，
///   未确认输入按既有策略丢弃不重放。
///   触控滚动：xterm 4.0.0 在 normal buffer + mouse tracking 下不转发滚轮（alt screen 内置
///   转发又是非标准 68/69 + 方向键回退），故包一层手势层，拖动经纯函数编码为
///   `CSI < 64/65 ; col ; row M` 帧走输入流；normal buffer 保持内置滚动，首次滚到顶触发
///   refreshHistory replay hydration。Git 动作复用 GitClient 既有 commit/merge/repairHookFailure。
class TerminalPage extends StatefulWidget {
  const TerminalPage({
    super.key,
    required this.book,
    required this.http,
    required this.project,
    this.preferredSessionId,
    this.worktreeId,
    this.worktreeInfo,
    this.worktreePath,
    this.onFullscreenChanged,
    this.onWorktreesMutated,
    this.onWorktreeOperationBusyChanged,
    this.confirmLeaveDirty,
    this.onActiveSessionChanged,
    this.sessionsRefreshToken = 0,
    @visibleForTesting this.sessionsClient,
    @visibleForTesting this.promptsClient,
    @visibleForTesting this.gitClient,
    @visibleForTesting this.eventsHttpClientFactory,
    @visibleForTesting this.backgroundTimersDisabled = false,
  });

  final AddressBook book;
  final LanHttpClient http;
  final ProjectSummary project;
  final String? preferredSessionId;
  final String? worktreeId;

  /// 当前 worktree 的权威 DTO（shell 从 worktrees 列表按 worktreeId 取出；
  /// 含 isMain/branch/homeBranch/canCollectMerge/status/path 等原始字段），
  /// 用于合并入口门控与确认文案（对齐 web canShowMobileTerminalMergeFab 的入参）。
  final Map<String, dynamic>? worktreeInfo;

  /// 当前 worktree 的磁盘路径：Prompt 优化 workingDirectory 优先使用（对齐 web worktree.path），
  /// 为空回退 project.path。
  final String? worktreePath;

  /// 进入/退出全屏时回调壳层（固定接缝契约，由 workbench_home 接线）。
  final ValueChanged<bool>? onFullscreenChanged;

  /// commit/merge 成功后通知壳层刷新 worktrees（固定接缝契约）。
  final VoidCallback? onWorktreesMutated;

  /// merge 全程（发起 → settle，含 unknown 对账）的壳层 worktree 操作互斥回调：
  /// true 置忙 / false 释放（try/finally 成对；多个来源并发由壳层计数收敛）。
  /// 壳层据此拒绝 worktree 切换并禁用 worktrees 页卡片——合并（将删源树）在途
  /// 期间不允许切到其它树（对齐 web beginWorktreeOperation 全局计数锁；
  /// 固定接缝契约，缺省 null 表示壳层未接（直连/测试））。
  final ValueChanged<bool>? onWorktreeOperationBusyChanged;

  /// 合并激活 worktree 前的脏文件预检接缝（由壳层注入，与 GitPage/WorktreesPage
  /// 同款签名）：返回 false 时合并中止且不调后端；缺省 null 表示壳层未接（直连/测试）。
  final Future<bool> Function(String worktreeId)? confirmLeaveDirty;

  /// 激活会话变化回调（固定接缝契约）：上报当前激活会话的权威 DTO（对齐 web
  /// onActiveSessionChange 传整个 session 对象）——壳层据此跟踪「当前真实会话」
  /// 用于离开终端面板/退出工作台时对旧会话补发 unfocus（终端页 Offstage 常驻不销毁，
  /// 壳层不能依赖 dispose），并取 [SessionSummary.displayName] 渲染状态行会话药丸
  /// （与 chip 条同源，对齐 web session={activeSession?.name}）。
  final ValueChanged<SessionSummary?>? onActiveSessionChanged;

  /// 壳层会话刷新令牌：Git 页/worktrees 页 worktree 删除/合并成功后 bump。
  /// didUpdateWidget 检测变化后拉当前项目权威会话列表，并 prune「本项目内已消失
  /// 会话」的常驻缓冲（对齐 web onRefreshSessions + removeBuffer 的收敛语义；
  /// 其它项目常驻缓冲不受影响）。默认 0 表示壳层未接（直连/测试）。
  final int sessionsRefreshToken;

  /// 测试注入：覆盖默认 SessionsClient。
  final SessionsClient? sessionsClient;

  /// 测试注入：覆盖默认 PromptsClient。
  final PromptsClient? promptsClient;

  /// 测试注入：覆盖默认 GitClient。
  final GitClient? gitClient;

  /// 测试注入：events NDJSON 流客户端工厂。提供时行流由该工厂返回的客户端产生，
  /// 页面不再自建/强关 HttpClient（断流中止由实现自身管理）；缺省保持原生行为
  /// （每轮新建 HttpClient，切会话/销毁时 force-close 断流）。
  final LanHttpClient Function()? eventsHttpClientFactory;

  /// 测试注入：关闭 events 空闲看门狗与两条通道的重连 Timer，避免 widget 测试挂起 Timer。
  final bool backgroundTimersDisabled;

  @override
  State<TerminalPage> createState() => _TerminalPageState();
}

class _TerminalPageState extends State<TerminalPage>
    with WidgetsBindingObserver {
  late final SessionsClient _sessions;
  late final PromptsClient _prompts;
  late final GitClient _git;
  final _view = xterm.TerminalController();
  final _input = TextEditingController();
  final _scrollController = ScrollController();
  WebSocket? _socket;
  HttpClient? _eventsClient;

  /// 常驻会话缓冲表：插入顺序即 LRU 顺序（最近使用重插尾部），上限
  /// [_kMaxMountedTerminalBuffers]，淘汰最久未用的非当前会话。
  final Map<String, _MountedSession> _mounted = <String, _MountedSession>{};

  /// 当前激活会话的缓冲；空态（尚无会话）为 null。
  _MountedSession? _active;

  /// 当前激活会话 id；与 [_active] 同步维护，空态为 null。
  String? _sessionId;
  String? _error;
  String _status = '连接中';
  String? _laneId;
  int _seq = 1;
  List<SessionSummary> _sessionList = [];
  bool _disposed = false;

  /// 面板常驻错误条文案（对齐 web panelError：前缀 + 可读详情，被覆盖前常驻）；
  /// 非空时若 [_panelErrorActionLabel] 也非空则附带动入口（如 hydration 重试）。
  String? _panelError;
  String? _panelErrorActionLabel;
  VoidCallback? _panelErrorAction;

  /// 划选操作条可见性（xterm 长按出现选区时显示，对齐 web selecting 底栏）。
  bool _selectionBarVisible = false;

  /// 当前选区覆盖的行数（操作条「已选 N 行」）。
  int _selectedLineCount = 0;

  /// 回前台跟随最新输出的 pin 截止时间；用户手动滚动（拖动）取消，null 表示未 pin。
  DateTime? _resumePinUntil;

  /// 通用动作占用：'create' | 'create-pane' | 'switch-pane' | 'close-pane' | 'commit'
  /// | 'merge' | 'repair' | 'close-会话id'，防重入。
  String? _actionBusy;

  // hook 失败修复卡状态（对齐 web MobileHookRepair）。
  _HookRepairState? _hookRepair;

  // commit/merge 的 mutation 相位机：unknown 后锁定动作并要求同 id 对账
  // （对齐 web commitPhase/mergePhase + pickMobileMutationOperationId）。
  final GitMutationTracker _commitMutation = GitMutationTracker();
  final GitMutationTracker _mergeMutation = GitMutationTracker();

  // 全屏状态：终端列放到根 Overlay，盖住壳层标题栏（对齐 web position:fixed 100dvh）。
  bool _fullscreen = false;
  final OverlayPortalController _fullscreenPortal = OverlayPortalController();

  // 收藏 Prompt 快捷输入：筛选条件跨打开保留（对齐 web 外层 state）。
  String _favoriteTag = _kFavoriteAllTag;
  String _favoriteQuery = '';

  // Prompt 优化提交中防重复。
  bool _optimizing = false;

  // 触控滚动转发（SGR wheel）：仅在 mouse tracking 已协商或 alt screen 时生效。
  bool _forwardWheel = false;
  final Set<int> _dragPointers = <int>{};
  TouchScrollState? _touchScroll;
  Size? _viewportSize;
  bool _measureScheduled = false;
  bool _terminalMeasured = false;
  int? _lastMeasuredCols;
  int? _lastMeasuredRows;
  Timer? _resizeDebounce;

  // events 长连接：代数用于让旧循环在切会话/销毁后立即失效。
  int _eventsGeneration = 0;
  int _eventsBackoffAttempt = 0;
  Timer? _eventsReconnectTimer;
  Timer? _eventsIdleTimer;
  Completer<void>? _eventsWake;
  bool _connectedOnce = false;
  bool _eventsDown = false;

  // 输入 WS：独立代数与退避。
  int _inputGeneration = 0;
  int _inputBackoffAttempt = 0;
  Timer? _inputReconnectTimer;

  // sticky Ctrl/Alt：3 秒无后续输入自动解除。
  StickyModifier? _sticky;
  final StickyModifierHold _stickyHold = StickyModifierHold();
  Timer? _stickyTimer;

  /// 当前激活会话的 DTO（来自最近一次列表刷新），无则 null。
  SessionSummary? get _currentSession {
    final id = _sessionId;
    if (id == null) {
      return null;
    }
    for (final session in _sessionList) {
      if (session.id == id) {
        return session;
      }
    }
    return null;
  }

  /// 会话 chip 条的作用域列表：projectId + worktreeId 过滤（对齐 web scopedSessions）。
  List<SessionSummary> get _scopedSessions => _sessionList
      .where(
        (session) => sessionMatchesWorktree(
          session,
          widget.worktreeId,
          projectId: widget.project.id,
        ),
      )
      .toList();

  /// 输入发送全路径门控（对齐 web inputEnabled + replayReady 门闩）：
  /// 会话已激活、权威状态 running、本会话 replay 门闩已放行、输入链路完成服务端
  /// ready 握手且 WS 仍处于 open。任一不满足一律丢弃输入（含 SGR wheel）。
  bool get _inputSendEnabled {
    final buffer = _active;
    final socket = _socket;
    return buffer != null &&
        _currentSession?.status == 'running' &&
        buffer.policy.replayReady &&
        buffer.policy.inputStreamReady &&
        socket != null &&
        socket.readyState == WebSocket.open;
  }

  /// 输入链路是否处于断开态（用于状态行文案）。
  bool get _inputLinkDown {
    final buffer = _active;
    return buffer != null &&
        buffer.policy.inputLink.state == TerminalInputLinkState.closed;
  }

  /// 断线时是否有未确认输入被丢弃（状态行区分两种断线提示）。
  bool get _inputDroppedUnacked =>
      _active?.policy.inputLink.droppedUnackedOnDisconnect ?? false;

  /// 贴图/收藏/优化统一门控（对齐 web canPasteImage/canOpenFavoriteQuickInput）：
  /// running + 输入流 ready + replay 门闩放行（收口到 [_inputSendEnabled] 同口径）。
  bool get _canUseInputActions => _inputSendEnabled;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    _sessions =
        widget.sessionsClient ??
        SessionsClient(widget.http, widget.book.active!.baseUrl);
    _prompts =
        widget.promptsClient ??
        PromptsClient(widget.http, widget.book.active!.baseUrl);
    _git =
        widget.gitClient ?? GitClient(widget.http, widget.book.active!.baseUrl);
    // 划选操作条跟随 xterm 选区出现/消失。
    _view.addListener(_onViewSelectionChanged);
    _boot();
  }

  @override
  void dispose() {
    _disposed = true;
    WidgetsBinding.instance.removeObserver(this);
    if (_fullscreenPortal.isShowing) {
      _fullscreenPortal.hide();
    }
    if (_fullscreen) {
      // 不能在 unmount 里直接 setState 壳层（树已锁）。下一帧再通知恢复标题栏。
      final notify = widget.onFullscreenChanged;
      WidgetsBinding.instance.addPostFrameCallback((_) {
        notify?.call(false);
      });
    }
    // 销毁时对当前会话补发 unfocus：停止旧远端窗口正文流（对齐 web effect cleanup
    // 的 compare-and-clear；fire-and-forget，失败静默）。
    final activeId = _sessionId;
    if (activeId != null) {
      _unfocusSession(activeId);
    }
    widget.onActiveSessionChanged?.call(null);
    _stickyTimer?.cancel();
    _resizeDebounce?.cancel();
    _eventsGeneration += 1;
    _cancelEventsReconnectWait();
    _eventsIdleTimer?.cancel();
    _inputGeneration += 1;
    _inputReconnectTimer?.cancel();
    _socket?.close();
    _eventsClient?.close(force: true);
    _view.removeListener(_onViewSelectionChanged);
    _mounted.clear();
    _active = null;
    _scrollController.dispose();
    _input.dispose();
    _view.dispose();
    super.dispose();
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (state != AppLifecycleState.resumed) {
      return;
    }
    _handleResumed();
  }

  @override
  void didUpdateWidget(covariant TerminalPage oldWidget) {
    super.didUpdateWidget(oldWidget);
    final projectChanged = oldWidget.project.id != widget.project.id;
    final worktreeChanged = oldWidget.worktreeId != widget.worktreeId;
    if (projectChanged || worktreeChanged) {
      _handleTerminalContextSwitch();
    } else if (oldWidget.preferredSessionId != widget.preferredSessionId) {
      // preferred 会话变化（attention 跳转 / hook 修复聚焦）：复用 boot 的
      // pickPreferredSession 优先级选中目标会话（已有缓冲切回，否则 replay）。
      unawaited(_boot());
    } else if (widget.sessionsRefreshToken != oldWidget.sessionsRefreshToken) {
      // 壳层会话刷新令牌变化（Git 页/worktrees 页删除·合并成功）：拉权威列表并
      // 清理本项目内已消失会话的常驻缓冲（对齐 web onRefreshSessions + removeBuffer）。
      unawaited(_pruneRemovedSessions());
    }
  }

  /// 业务逻辑：Git 页/worktrees 页删除·合并 worktree 成功后，源树的终端窗口已被
  /// 服务端关闭；壳层 bump 会话刷新令牌后本页必须拉权威列表并清理「本项目内已消失
  /// 会话」的常驻缓冲，避免后续 resize/输入打到不存在的会话（对齐 web
  /// pruneMobileSessionsForClosedWorktree + removeBuffer）。其它项目的常驻缓冲
  /// 跨项目保留，不受影响。
  ///
  /// Code Logic: 拉当前项目权威列表（失败静默保留现状，下次令牌变化或回前台再收敛）
  /// → setState 写 _sessionList → 按「projectId 相同且 id 不在权威列表」收集待清理
  /// buffer 并移除；若被清理的是当前激活会话，按既有「无会话/切换优先会话」路径收敛
  /// （pickPreferredSession → _activateSession 或空态）。
  Future<void> _pruneRemovedSessions() async {
    List<SessionSummary> sessions;
    try {
      sessions = await _sessions.list(widget.project.id);
    } catch (_) {
      return;
    }
    if (!mounted || _disposed) {
      return;
    }
    setState(() => _sessionList = sessions);
    final validIds = sessions.map((session) => session.id).toSet();
    final staleIds = [
      for (final entry in _mounted.entries)
        if (entry.value.projectId == widget.project.id &&
            !validIds.contains(entry.key))
          entry.key,
    ];
    if (staleIds.isEmpty) {
      return;
    }
    setState(() {
      for (final id in staleIds) {
        final buffer = _mounted.remove(id);
        buffer?.terminal.removeListener(_onTerminalStateMaybeChanged);
      }
    });
    final activeId = _sessionId;
    if (activeId != null && staleIds.contains(activeId)) {
      // 激活会话被外部删除：与关闭当前会话同款收敛路径。
      _sessionId = null;
      final nextSession = pickPreferredSession(
        sessions,
        worktreeId: widget.worktreeId,
      );
      if (nextSession != null) {
        await _activateSession(nextSession);
      } else {
        await _showEmptyState();
      }
    }
  }

  /// 业务逻辑：终端面板跨项目 hidden 常驻（对齐 web terminal 面板常驻 +
  /// backgroundSessions 跨项目保留输出缓冲）：切换项目/worktree 时本页 State 不再
  /// 重建，必须显式断开旧上下文并按新上下文重新 boot；`_mounted` 常驻缓冲按
  /// sessionId 跨项目保留（LRU 8 一视同仁），切回原项目命中缓冲不清屏不重放。
  ///
  /// Code Logic：对旧会话 unfocus（停止远端窗口正文流）并丢弃未确认输入；递增两条
  /// 通道代数断开 events 流与输入 WS；清空会话列表/错误/门闩等页面级视图状态
  /// （commit/merge unknown 锁与 hook 失败卡同 web 一并不带入新上下文）；随后走既有
  /// boot 流程按新 project/worktree 拉会话并选优先会话（命中缓冲切回、否则 replay）。
  /// worktree strip 回调、全屏回调、dirty 接缝等全部来自 widget 参数，本帧起自然更新。
  void _handleTerminalContextSwitch() {
    final previousId = _sessionId;
    _stopEventsLoop();
    _eventsIdleTimer?.cancel();
    _active?.policy.takeUnackedOnDisconnect();
    if (previousId != null) {
      _unfocusSession(previousId);
    }
    // 输入 WS：递增代数让旧回调失效并关闭连接（未确认输入已丢弃，不重放）。
    _inputGeneration += 1;
    _inputReconnectTimer?.cancel();
    _inputReconnectTimer = null;
    _socket?.close();
    _socket = null;
    _view.clearSelection();
    if (!mounted || _disposed) {
      return;
    }
    setState(() {
      _sessionId = null;
      _active = null;
      _sessionList = <SessionSummary>[];
      _error = null;
      _status = '连接中';
      _panelError = null;
      _panelErrorActionLabel = null;
      _panelErrorAction = null;
      _connectedOnce = false;
      _eventsDown = false;
      _selectionBarVisible = false;
      _selectedLineCount = 0;
      _resumePinUntil = null;
      _actionBusy = null;
      _hookRepair = null;
      _laneId = null;
      _seq = 1;
    });
    // 切换项目/worktree 后不得把旧上下文的 mutation unknown 锁带入新上下文
    // （对齐 web context 切换 effect 的 setCommitPhase/mergePhase idle 重置）。
    _commitMutation.reset();
    _mergeMutation.reset();
    unawaited(_boot());
  }

  /// 业务逻辑：手机回前台后半开连接无法探测，输入通道必须立即重建、事件流立即重连一次；
  /// 同时视口应跟随最新输出（对齐 web mobileTerminalResumeFollow：8s pin 窗口）。
  ///
  /// Code Logic：输入直接重连（重连 Timer 若在等待则被取消）；events 若在退避等待中
  /// 则立即唤醒并重置退避，若仍在连接中则强制断开交给循环走重连分支；随后静默刷新
  /// 一次会话列表。回前台时若未在划选、无选区，把视口钉到底并在 8s 窗口内随
  /// catch-up 跟随（xterm RenderTerminal 在底部时自动钉底）；用户拖动滚动经
  /// NotificationListener 取消 pin。由回前台强制的 events 重连成功后还会在首帧处
  /// 再静默刷一次（与退避重连共用该钩子），两次均为幂等读。
  void _handleResumed() {
    if (!mounted || _disposed) {
      return;
    }
    unawaited(_connectInput());
    final wake = _eventsWake;
    if (wake != null && !wake.isCompleted) {
      _eventsBackoffAttempt = 0;
      wake.complete();
    } else {
      _eventsClient?.close(force: true);
    }
    // 回前台跟随最新输出：划选/有选区时不抢滚动（对齐 web
    // shouldFollowMobileTerminalToLatest）。
    if (_active != null && _view.selection == null && !_selectionBarVisible) {
      _resumePinUntil = DateTime.now().add(
        const Duration(milliseconds: _kResumeFollowMs),
      );
      _jumpToBottom();
    }
    // 回前台后静默刷新会话列表；刷新失败静默（_refreshSessions 自吞异常）。
    unawaited(_refreshSessions());
  }

  /// 把终端视口跳到最新输出（帧末执行，等 scrollExtent 稳定）。
  void _jumpToBottom() {
    WidgetsBinding.instance.addPostFrameCallback((_) {
      final scroll = _scrollController;
      if (_disposed || !scroll.hasClients) {
        return;
      }
      scroll.jumpTo(scroll.position.maxScrollExtent);
    });
  }

  /// 业务逻辑：boot 是页面可重入的初始化入口（initState 首启与错误页「重试」共用）：
  /// 拉会话列表 → 选首选会话 → 激活；无会话时展示空态 + 手动新建，不再自动创建
  /// （对齐 web：无会话显示「当前 worktree 还没有终端窗口」+「新窗口」按钮）。
  ///
  /// Code Logic：先清掉上一次的 _error（initState 首次调用时为空，跳过 setState），
  /// 随后 list → pickPreferredSession（带 worktreeId 作用域优先级）→ 命中则
  /// _activateSession；异常统一落 _error，由 build 渲染错误页与重试按钮。
  Future<void> _boot() async {
    if (_error != null && mounted && !_disposed) {
      setState(() => _error = null);
    }
    try {
      final sessions = await _sessions.list(widget.project.id);
      if (!mounted || _disposed) {
        return;
      }
      setState(() => _sessionList = sessions);
      final session = pickPreferredSession(
        sessions,
        preferredId: widget.preferredSessionId,
        worktreeId: widget.worktreeId,
      );
      if (session != null) {
        await _activateSession(session);
      }
    } catch (error) {
      if (mounted) {
        setState(() => _error = error.toString());
      }
    }
  }

  /// fire-and-forget unfocus：停止该会话的远端窗口正文流过滤目标
  /// （对齐 web cleanup 里 sessions.focus(sessionId, false)；失败静默，
  /// 下一次 focus 会以当前窗口重新建立过滤目标）。
  void _unfocusSession(String sessionId) {
    unawaited(
      _sessions.focus(sessionId, streamActive: false).catchError((_) {}),
    );
  }

  /// 业务逻辑：切换会话时新画面来自目标会话的常驻缓冲——首次激活清屏 + replay 快照
  /// 建立基线；切回已缓冲会话不清屏不重放，靠 events 增量（afterSequence）续传，
  /// 序列过旧由 gap 帧兜底全量重放（对齐 web 常驻 xterm + 增量续传）。
  ///
  /// Code Logic：先停掉旧 events 循环、丢弃旧会话未确认输入，再 focus 远端（失败上屏
  /// 错误条）、zoom-pane 幂等（失败上屏）；随后取/建目标缓冲并清屏 + replay（仅新
  /// buffer），成败都放行 replayReady；切会话保持全屏（对齐 web），最后重建输入 WS
  /// 并重启 events 循环，刷新列表。划选状态随切会话清除。
  Future<void> _activateSession(SessionSummary session) async {
    final nextId = session.id;
    if (nextId.isEmpty ||
        !shouldResetSessionView(
          currentSessionId: _sessionId,
          nextSessionId: nextId,
        )) {
      // 同一会话重复点击不重复清屏重放。
      return;
    }
    final previousId = _sessionId;
    _stopEventsLoop();
    // 旧会话未确认输入结果未知：切走即丢弃（不重放、不写入 /sessions/write）。
    _active?.policy.takeUnackedOnDisconnect();
    // 切走旧会话即停止其远端窗口正文流（对齐 web effect cleanup；fire-and-forget）。
    if (previousId != null) {
      _unfocusSession(previousId);
    }
    // 对齐 web focusSessionAndZoomById 首行：目标会话非 running 时跳过 focus/zoom
    // RPC——exited 会话仍可切换查看终态输出（replay/清屏照常），且不产生该路径的
    // 「切换终端失败」错误条。
    if (session.status == 'running') {
      try {
        await _sessions.focus(nextId);
      } catch (error) {
        _setPanelError('切换终端失败：$error');
      }
    }
    // 移动端单屏只能展示一个 pane：切会话后把 tmux 分屏收成 zoom 单 pane（幂等；
    // _ensurePaneZoomed 内部同样按 running 门控跳过）。
    await _ensurePaneZoomed(session);
    if (!mounted || _disposed) {
      return;
    }
    var buffer = _mounted[nextId];
    final isNewBuffer = buffer == null;
    if (buffer == null) {
      buffer =
          _MountedSession(
              nextId,
              projectId: widget.project.id,
              onOutput: _handleTerminalOutput,
            )
            ..persistedCols = session.cols
            ..persistedRows = session.rows;
      _mounted[nextId] = buffer;
      _evictMountedBuffers();
    } else {
      // LRU 触碰：重插尾部，避免切回常用会话被淘汰。
      _mounted.remove(nextId);
      _mounted[nextId] = buffer;
    }
    buffer.terminal.addListener(_onTerminalStateMaybeChanged);
    _view.clearSelection();
    setState(() {
      _sessionId = nextId;
      _active = buffer;
      _connectedOnce = false;
      _eventsDown = false;
      _status = '连接中';
      _panelError = null;
      _panelErrorActionLabel = null;
      _panelErrorAction = null;
      _selectionBarVisible = false;
      _selectedLineCount = 0;
      _resumePinUntil = null;
      // 切换会话保持全屏（对齐 web：isTerminalFullscreen 只随 visibleSession 存在性变化）。
    });
    buffer.policy.onInputLinkChanged = _onInputLinkChanged;
    // 壳层接缝：上报当前真实激活会话的权威 DTO（壳层取 id 做 unfocus 跟踪、
    // 取 displayName 渲染状态行会话药丸，与 chip 条同源）。
    widget.onActiveSessionChanged?.call(session);
    if (isNewBuffer) {
      // 首次激活：清屏 + replay 快照建立基线；完成（成功或失败）前输入门闩保持关闭。
      buffer.terminal.write('\x1b[3J\x1b[2J\x1b[H');
      try {
        final replay = await _sessions.replay(nextId);
        if (!mounted || _disposed || !identical(_active, buffer)) {
          return;
        }
        final snapshot = _replaySnapshotOf(replay);
        if (snapshot.isNotEmpty) {
          buffer.terminal.write(snapshot);
        }
      } catch (error) {
        if (!mounted || _disposed || !identical(_active, buffer)) {
          return;
        }
        // 快照拉取失败时仍以实时流继续，gap 帧会触发再次重放。会话已关闭类失败
        // （404/not-found）属预期态：静默吞掉不置错误条（对齐 web XtermSlot
        // replay catch 先 isExpectedClosedSessionError 判定即静默 return）；
        // 其余失败仍上屏错误条（对齐 web setPanelError）。注意只跳过错误条，
        // 不得提前返回函数——后续输入 WS 重建/events 循环必须照常执行。
        if (!_isClosedSessionError(error)) {
          _setPanelError('加载终端历史失败：$error');
        }
      } finally {
        // 成败都置 replayReady（对齐 web then/catch 两分支均置 replayReady=true）。
        buffer.policy.finishReplay();
        // 对齐 web replay.then/catch 再次 startHydrationRequest 的兜底：replay
        // 在途期间累计的上滑 hydration 意图已达阈值时，replay 一完成就自动触发。
        if (mounted &&
            !_disposed &&
            identical(_active, buffer) &&
            buffer.hydrationIntent <= -1) {
          unawaited(_beginHistoryHydration(buffer));
        }
      }
      _jumpToBottom();
    }
    await _openInput();
    _startEventsLoop();
    await _refreshSessions();
  }

  /// LRU 淘汰：超过上限时从最旧开始淘汰非当前会话（对齐 web
  /// nextMountedMobileSessionIds：active 始终保留）。
  void _evictMountedBuffers() {
    while (_mounted.length > _kMaxMountedTerminalBuffers) {
      String? evictId;
      for (final id in _mounted.keys) {
        if (id != _sessionId) {
          evictId = id;
          break;
        }
      }
      if (evictId == null) {
        return;
      }
      final evicted = _mounted.remove(evictId);
      evicted?.terminal.removeListener(_onTerminalStateMaybeChanged);
    }
  }

  /// 输入链路状态变化：刷新状态行/输入禁用态；封锁原因上屏错误条
  /// （对齐 web input stream error → setPanelError）；链路回到 ready 时清除
  /// 历史 blocked 错误条，避免恢复后仍显示「终端输入连接失败」（对齐 web
  /// ready → setPanelError(null)）。
  void _onInputLinkChanged(TerminalInputLinkStatus status) {
    if (!mounted || _disposed) {
      return;
    }
    if (status.state == TerminalInputLinkState.blocked &&
        status.message != null) {
      _setPanelError(status.message!);
      return;
    }
    if (status.state == TerminalInputLinkState.ready) {
      _setPanelError(null);
      return;
    }
    _refreshStatus();
  }

  /// 面板常驻错误条：写入可读错误（对齐 web panelError = 前缀 + getErrorMessage，
  /// 被覆盖前常驻）；message 为 null 时清除。可选附带一个动作用户（如 hydration 重试）。
  void _setPanelError(
    String? message, {
    String? actionLabel,
    VoidCallback? action,
  }) {
    if (_disposed) {
      return;
    }
    if (!mounted) {
      return;
    }
    setState(() {
      _panelError = message;
      _panelErrorActionLabel = message == null ? null : actionLabel;
      _panelErrorAction = message == null ? null : action;
    });
  }

  /// 刷新会话列表；失败静默（列表刷新失败不影响已完成的切换）。
  Future<void> _refreshSessions() async {
    try {
      final sessions = await _sessions.list(widget.project.id);
      if (mounted && !_disposed) {
        setState(() => _sessionList = sessions);
      }
    } catch (_) {}
  }

  /// replay 响应中提取快照文本：后端权威字段为 buffer（WorkbenchSessionReplayDto
  /// camelCase 投影，web 同读 replay.buffer），snapshot/data/output 为旧后端宽容兜底。
  String _replaySnapshotOf(Map<String, dynamic> replay) {
    return replay['buffer'] as String? ??
        replay['snapshot'] as String? ??
        replay['data'] as String? ??
        replay['output'] as String? ??
        '';
  }

  /// replay 响应中提取快照基线序列号（camelCase 为主、snake_case 兜底）；
  /// 缺失返回 null，表示无法按 seq 过滤暂存 chunk（全部按序补写）。
  int? _replayLastSeqOf(Map<String, dynamic> replay) {
    final seq = replay['lastSeq'] ?? replay['last_seq'];
    if (seq is int) {
      return seq;
    }
    return seq is num ? seq.toInt() : null;
  }

  /// replay 响应中提取快照 ownerInstanceId（camelCase 为主、snake_case 兜底）；
  /// 缺失返回 null（与无主 chunk 视为同 authority，对齐 web `?? null` 比较）。
  String? _replayOwnerOf(Map<String, dynamic> replay) {
    final owner = replay['ownerInstanceId'] ?? replay['owner_instance_id'];
    return owner is String && owner.isNotEmpty ? owner : null;
  }

  /// pane 操作通用门控：session 支持 panes 且无动作占用（对齐 web canRunMobilePaneMutation）。
  bool get _canRunPaneMutation {
    final session = _currentSession;
    return session != null && session.supportsPanes && _actionBusy == null;
  }

  /// 切换 pane 额外要求 paneCount > 1（单 pane 切换无可见效果，对齐 web）。
  bool get _canSwitchPane =>
      _canRunPaneMutation && (_currentSession?.paneCount ?? 0) > 1;

  /// 业务逻辑：移动端单屏只能展示一个 pane，进入多 pane window 后必须收成 zoom 单 pane。
  ///
  /// Code Logic：仅对 running 且支持 panes 的会话调用 zoom-pane；后端幂等，失败上屏
  /// 错误条（对齐 web ensurePaneZoomedById catch → setPanelError）。
  Future<void> _ensurePaneZoomed(SessionSummary session) async {
    if (session.status != 'running' || !session.supportsPanes) {
      return;
    }
    try {
      await _sessions.zoomPane(session.id);
    } catch (error) {
      _setPanelError('切换单 pane 视图失败：$error');
    }
  }

  /// 业务逻辑：手机端新增 pane 必须由 tmux 创建真实 pane，方向固定向下（对齐 web）。
  ///
  /// Code Logic：split-pane(down) → zoom 收单 pane → 刷新列表；全部动作 busy 防重入；
  /// 失败写入面板错误条（前缀 + 服务端错误详情，对齐 web handleCreatePane）。
  Future<void> _createPane() async {
    final session = _currentSession;
    if (session == null || !session.supportsPanes || _actionBusy != null) {
      return;
    }
    setState(() => _actionBusy = 'create-pane');
    _setPanelError(null);
    try {
      await _sessions.splitPane(session.id, direction: 'down');
      await _ensurePaneZoomed(session);
      await _refreshSessions();
    } catch (error) {
      _setPanelError('分屏失败：$error');
    } finally {
      if (mounted && !_disposed) {
        setState(() => _actionBusy = null);
      }
    }
  }

  /// 业务逻辑：多 pane window 中一键切到下一个 pane，避免用户手敲 tmux 快捷键。
  ///
  /// Code Logic：switch-pane → zoom 幂等；仅 paneCount > 1 时可用；失败上屏错误条。
  Future<void> _switchPane() async {
    final session = _currentSession;
    if (session == null || !_canSwitchPane) {
      return;
    }
    setState(() => _actionBusy = 'switch-pane');
    _setPanelError(null);
    try {
      await _sessions.switchPane(session.id);
      await _ensurePaneZoomed(session);
    } catch (error) {
      _setPanelError('切换 pane 失败：$error');
    } finally {
      if (mounted && !_disposed) {
        setState(() => _actionBusy = null);
      }
    }
  }

  /// 业务逻辑：关闭 pane 应映射到真实 tmux pane；关掉最后一个 pane 时窗口随之移除。
  ///
  /// Code Logic：close-pane；closedWindow=true 时本地移除该会话并按优先级选下一个
  /// （同 worktree 作用域，pickPreferredSession）走既有切换流程；false 分支为幂等
  /// zoom-pane + 刷新会话列表（对齐 web handleClosePane else 分支）。失败上屏错误条。
  Future<void> _closePane() async {
    final session = _currentSession;
    if (session == null || !session.supportsPanes || _actionBusy != null) {
      return;
    }
    setState(() => _actionBusy = 'close-pane');
    _setPanelError(null);
    try {
      final result = await _sessions.closePane(session.id);
      if (result.closedWindow) {
        final next = _scopedSessions
            .where((item) => item.id != result.sessionId)
            .toList();
        if (!mounted || _disposed) {
          return;
        }
        setState(() {
          _sessionList = _sessionList
              .where((item) => item.id != result.sessionId)
              .toList();
        });
        if (_sessionId == result.sessionId) {
          _sessionId = null;
          final nextSession = pickPreferredSession(
            next,
            worktreeId: widget.worktreeId,
          );
          if (nextSession != null) {
            await _activateSession(nextSession);
          } else {
            await _showEmptyState();
          }
        } else {
          await _ensurePaneZoomed(session);
        }
        await _refreshSessions();
      } else {
        // 仅关 pane 未关窗口：zoom-pane 幂等收拢 + 刷新权威列表（对齐 web）。
        await _ensurePaneZoomed(session);
        await _refreshSessions();
      }
    } catch (error) {
      _setPanelError('关闭 pane 失败：$error');
    } finally {
      if (mounted && !_disposed) {
        setState(() => _actionBusy = null);
      }
    }
  }

  /// 业务逻辑：手机端需要能关闭终端窗口（当前或非当前、含 exited），释放后端 PTY。
  ///
  /// Code Logic：调 sessions/close；关当前会话 → 本地移除 → 同 worktree 作用域
  /// pickPreferredSession 选下一个 → 既有切换流程，无剩余会话进入空态；关其他会话 →
  /// 仅移除 chip 后刷新列表；失败上屏错误条。
  Future<void> _closeSession(SessionSummary session) async {
    if (_actionBusy != null) {
      return;
    }
    setState(() => _actionBusy = 'close-${session.id}');
    _setPanelError(null);
    try {
      await _sessions.close(session.id);
      final next = _scopedSessions
          .where((item) => item.id != session.id)
          .toList();
      if (!mounted || _disposed) {
        return;
      }
      setState(() {
        _sessionList = _sessionList
            .where((item) => item.id != session.id)
            .toList();
      });
      if (_sessionId == session.id) {
        _sessionId = null;
        final nextSession = pickPreferredSession(
          next,
          worktreeId: widget.worktreeId,
        );
        if (nextSession != null) {
          await _activateSession(nextSession);
        } else {
          await _showEmptyState();
        }
      }
      await _refreshSessions();
    } catch (error) {
      _setPanelError('关闭窗口失败：$error');
    } finally {
      if (mounted && !_disposed) {
        setState(() => _actionBusy = null);
      }
    }
  }

  /// 业务逻辑：当前会话被关闭/合并掉且没有下一个会话时，进入空态并退出全屏
  /// （对齐 web：isTerminalFullscreen = fullscreen && visibleSession !== null）。
  Future<void> _showEmptyState() async {
    if (!mounted || _disposed) {
      return;
    }
    final wasFullscreen = _fullscreen;
    if (wasFullscreen && _fullscreenPortal.isShowing) {
      _fullscreenPortal.hide();
    }
    setState(() {
      _active = null;
      _status = '连接中';
      if (wasFullscreen) {
        _fullscreen = false;
      }
    });
    // 空态下无激活会话，同步壳层的会话跟踪。
    widget.onActiveSessionChanged?.call(null);
    if (wasFullscreen) {
      widget.onFullscreenChanged?.call(false);
    }
    _stopEventsLoop();
  }

  /// 业务逻辑：终端区域尺寸变化必须同步远端 PTY（web ResizeObserver 同语义），否则 TUI 错位。
  ///
  /// Code Logic：LayoutBuilder 记录视口尺寸，帧末读取 xterm autoResize 后的 viewWidth/viewHeight，
  /// clamp 后与最近上报基线不同才经 80ms 防抖调 sessions/resize。基线按会话保留在
  /// buffer.lastSentResize；基线缺失时优先以服务端持久化 session.cols/rows 初始化
  /// （后端把同尺寸 resize 当强制重绘、会把 TUI 末屏抖进 tmux history，对齐 web
  /// XtermSlot persistedSessionSize：相同不回传）。首次测量成功后记录实测尺寸，
  /// 新建会话时据此携带 initialCols/initialRows。
  void _scheduleMeasure(Size size) {
    _viewportSize = size;
    if (_measureScheduled) {
      return;
    }
    _measureScheduled = true;
    WidgetsBinding.instance.addPostFrameCallback((_) {
      _measureScheduled = false;
      if (_disposed || !mounted) {
        return;
      }
      final viewport = _viewportSize;
      if (viewport == null || viewport.height <= 0 || viewport.width <= 0) {
        return;
      }
      _terminalMeasured = true;
      final buffer = _active;
      if (buffer == null) {
        return;
      }
      final sessionId = buffer.sessionId;
      final cols = clampTerminalDimension(
        buffer.terminal.viewWidth,
        kMinTerminalCols,
      );
      final rows = clampTerminalDimension(
        buffer.terminal.viewHeight,
        kMinTerminalRows,
      );
      _lastMeasuredCols = cols;
      _lastMeasuredRows = rows;
      if (buffer.lastSentResize == null &&
          buffer.persistedCols != null &&
          buffer.persistedRows != null) {
        // 服务端持久化尺寸作为已上报基线：首帧 fit 相同就不回传 resize。
        buffer.lastSentResize = (
          sessionId,
          clampTerminalDimension(buffer.persistedCols!, kMinTerminalCols),
          clampTerminalDimension(buffer.persistedRows!, kMinTerminalRows),
        );
      }
      final last = buffer.lastSentResize;
      if (last != null &&
          last.$1 == sessionId &&
          last.$2 == cols &&
          last.$3 == rows) {
        return;
      }
      _resizeDebounce?.cancel();
      _resizeDebounce = Timer(const Duration(milliseconds: 80), () {
        if (_disposed || !identical(_active, buffer)) {
          return;
        }
        buffer.lastSentResize = (sessionId, cols, rows);
        unawaited(
          _sessions
              .resize(sessionId, cols, rows)
              .then<void>(
                (_) {},
                onError: (Object error) {
                  if (_disposed || !mounted) {
                    return;
                  }
                  // 会话已关闭（404/not-found 类）属预期：静默吞掉（对齐 web
                  // isExpectedClosedSessionError）；其余失败上屏常驻错误条。
                  if (_isClosedSessionError(error)) {
                    return;
                  }
                  _setPanelError('调整终端尺寸失败：$error');
                },
              ),
        );
      });
    });
  }

  /// 判定错误是否为「会话已关闭」类（对齐 web classifyTerminalReplayError 的
  /// not_found 分支）：LanHttpException 404 或错误文案含 not-found 语义时视为
  /// 会话已不存在，resize/replay 类失败静默。
  bool _isClosedSessionError(Object error) {
    if (error is LanHttpException) {
      return error.statusCode == 404;
    }
    final message = error.toString().toLowerCase();
    return message.contains('404') ||
        message.contains('not found') ||
        message.contains('not_found') ||
        message.contains('session_not_found');
  }

  /// 业务逻辑：xterm buffer/mouse 状态随输出变化，滚动转发门控翻转时需要重建手势层；
  /// 同时回前台 pin 窗口内的 catch-up 输出要跟随最新行。
  ///
  /// Code Logic：terminal notifyListeners 时重算 _forwardWheel；只有翻转才 setState，
  /// 避免高频输出触发无谓重建。pin 窗口内且视口未在底部时帧末钉回底部；用户拖动
  /// （ScrollUpdateNotification.dragDetails 非空）已把 pin 置空。
  void _onTerminalStateMaybeChanged() {
    final next = _computeForwardWheel();
    if (next != _forwardWheel && mounted && !_disposed) {
      setState(() => _forwardWheel = next);
    }
    if (_selectionBarVisible) {
      _refreshSelectionBarState();
    }
    // 回前台 8s pin 窗口：catch-up 输出持续把视口钉在最新行（xterm 在底部时本就
    // 自动钉底；此处兜底「跳底后首帧 catch-up 尚未写入」的场景）。
    final pinUntil = _resumePinUntil;
    if (pinUntil != null &&
        mounted &&
        !_disposed &&
        DateTime.now().isBefore(pinUntil) &&
        _scrollController.hasClients &&
        _scrollController.position.pixels <
            _scrollController.position.maxScrollExtent) {
      _jumpToBottom();
    }
  }

  /// 滚动转发判定：mouse tracking 已协商或处于 alternate screen 时把拖动编码为 SGR wheel。
  bool _computeForwardWheel() {
    final terminal = _active?.terminal;
    if (terminal == null) {
      return false;
    }
    return terminal.mouseMode != MouseMode.none || terminal.isUsingAltBuffer;
  }

  /// 业务逻辑：转发模式下单指拖动即 TUI 滚轮；非转发模式下首次向上拖动即累计
  /// hydration 意图（未灌历史时触发，对齐 web 首次上滑即触发）。
  ///
  /// Code Logic：仅跟踪单指 touch；行高按视口/rows 换算；转发模式按触点落格
  /// （sgrWheelCellFromTouch）编码 `CSI < 64/65 ; col ; row M`（单次最多 8 帧）走
  /// _send；普通模式对向上意图累计，≤ -1 行且 replay 门闩已放行时触发
  /// refreshHistory hydration（去掉「须先滚到顶」前置）；replay 在途时意图照常
  /// 累计（对齐 web pendingHydratedScrollLines 排队），完成后的兜底自动触发。
  void _onSurfacePointerDown(PointerDownEvent event) {
    if (event.kind != PointerDeviceKind.touch) {
      return;
    }
    _dragPointers.add(event.pointer);
    if (_dragPointers.length == 1) {
      _touchScroll = TouchScrollState(lastClientY: event.position.dy);
    } else {
      // 多指触摸不转发，避免抖动式滚轮轰炸 TUI。
      _touchScroll = null;
    }
  }

  void _onSurfacePointerMove(PointerMoveEvent event) {
    if (!_dragPointers.contains(event.pointer) || _dragPointers.length != 1) {
      return;
    }
    final state = _touchScroll;
    final buffer = _active;
    if (state == null || buffer == null) {
      return;
    }
    final terminal = buffer.terminal;
    final lineHeight = terminalTouchLineHeight(
      _viewportSize?.height ?? 0,
      terminal.viewHeight,
      0,
    );
    final result = updateTouchScroll(state, event.position.dy, lineHeight);
    _touchScroll = result.state;
    if (result.lines == 0) {
      return;
    }
    if (_forwardWheel) {
      // 触点换算为 1-based 字符格（贴近桌面滚轮落点；失败回落 1,1，对齐 web）。
      final local = event.localPosition;
      final cell = sgrWheelCellFromTouch(
        localDx: local.dx,
        localDy: local.dy,
        viewportWidth: _viewportSize?.width ?? 0,
        viewportHeight: _viewportSize?.height ?? 0,
        cols: clampTerminalDimension(terminal.viewWidth, kMinTerminalCols),
        rows: clampTerminalDimension(terminal.viewHeight, kMinTerminalRows),
      );
      final wheel = encodeSgrWheelReports(
        result.lines,
        col: cell.col,
        row: cell.row,
      );
      if (wheel.isNotEmpty) {
        _send(wheel);
      }
      return;
    }
    // 普通滚动：向上（lines<0）拖动即累计意图，首次上滑即触发 hydration（不再要求
    // 先滚到顶；对齐 web resolveMobileTerminalScrollMode 的 hydrateScrollback 分支）。
    if (result.lines >= 0) {
      return;
    }
    final hydrated = buffer.policy.isHistoryHydrated(
      ownerInstanceId: buffer.owner,
    );
    if (hydrated || buffer.hydrating) {
      return;
    }
    // replay 在途（门闩未放行）时意图照常累计不丢弃，等 replay 完成后由
    // _activateSession 兜底自动触发（对齐 web pendingHydratedScrollLines 排队
    // + replay.then 再次 startHydrationRequest）；已就绪才立即触发 hydration。
    buffer.hydrationIntent = accumulateHydrationScrollIntent(
      buffer.hydrationIntent,
      result.lines,
      terminal.viewHeight,
    );
    if (buffer.hydrationIntent <= -1 && buffer.policy.replayReady) {
      unawaited(_beginHistoryHydration(buffer));
    }
  }

  void _onSurfacePointerUp(PointerEvent event) {
    _dragPointers.remove(event.pointer);
    if (_dragPointers.isEmpty) {
      _touchScroll = null;
    }
  }

  /// 业务逻辑：tmux 里的 resume 旧消息不在本地 replay 快照中，首次回看历史必须显式
  /// refreshHistory 拉含 tmux 历史的快照并替换当前 buffer。
  ///
  /// Code Logic：单飞门闩（buffer.hydrating）；是否已灌按会话 + ownerInstanceId 判定
  /// （/resume 换 owner 后允许重灌，对齐 web hydratedScrollbackSessionRef + owner）；记录
  /// 替换前「距底部行距」作锚点；成功后清屏（含 scrollback 擦除）+ 写入快照 + 按序补写
  /// 往返期间暂存的 live chunk（seq > 快照 lastSeq 且 owner 一致，对齐 web
  /// appendHeldLiveAfterReplay），帧末把视口钉回相同底部锚点并随本次触发手势至少
  /// 上滚 1 行进入刚灌入的历史（对齐 web scrollWhenHydrationParsed）；失败不标记 hydrated、
  /// 先补写暂存 chunk（防丢失），会话已关闭类失败静默，其余错误上屏并附「重试」，
  /// live 流继续不受影响。
  Future<void> _beginHistoryHydration(_MountedSession buffer) async {
    if (buffer.hydrating ||
        buffer.policy.isHistoryHydrated(ownerInstanceId: buffer.owner)) {
      return;
    }
    buffer.hydrating = true;
    final sessionId = buffer.sessionId;
    final scroll = _scrollController;
    final distFromBottom = scroll.hasClients
        ? scroll.position.maxScrollExtent - scroll.position.pixels
        : null;
    try {
      final replay = await _sessions.hydrateScrollback(
        sessionId,
        timeout: const Duration(milliseconds: kHistoryHydrationTimeoutMs),
      );
      if (_disposed) {
        return;
      }
      if (!mounted || !identical(_active, buffer)) {
        // 会话在往返期间被切走：快照作废（下次回看重新 hydration），但暂存的
        // live chunk 仍要全部按序补写进常驻缓冲——这些帧的 seq 已推进事件基线，
        // 不补写就会永久丢失。
        _flushHeldLiveChunks(buffer);
        return;
      }
      final snapshot = _replaySnapshotOf(replay);
      final snapshotLastSeq = _replayLastSeqOf(replay);
      buffer.terminal.write('\x1b[3J\x1b[2J\x1b[H');
      if (snapshot.isNotEmpty) {
        buffer.terminal.write(snapshot);
      }
      // 往返期间到达的实时输出按序补写在权威快照之后（只补 owner 一致且
      // seq > 快照 lastSeq 的 chunk），随后恢复实时写。
      _flushHeldLiveChunks(
        buffer,
        afterSeq: snapshotLastSeq,
        snapshotOwner: _replayOwnerOf(replay),
      );
      if (snapshotLastSeq != null && snapshotLastSeq > buffer.sequence) {
        // 快照已包含比本地 events 基线更新的序列：推进基线，避免断线重连按旧
        // afterSequence 重拉已随快照上屏的内容（对齐 web store.reset 设新 cursor）。
        buffer.sequence = snapshotLastSeq;
      }
      // 本次触发手势的滚动意图随灌入生效：视口至少上滚 1 行进入历史
      // （对齐 web Math.min(-1, pendingHydratedScrollLines)）。
      final scrollUpLines = buffer.hydrationIntent < -1
          ? -buffer.hydrationIntent
          : 1;
      buffer.policy.markHistoryHydrated(ownerInstanceId: buffer.owner);
      buffer.hydrationIntent = 0;
      _setPanelError(null);
      if (distFromBottom != null) {
        _anchorViewportFromBottom(distFromBottom, extraUpLines: scrollUpLines);
      }
    } catch (error) {
      // 失败先按序补写暂存 chunk（防丢失，不标记 hydrated）；live 流继续不受影响。
      _flushHeldLiveChunks(buffer);
      if (mounted && !_disposed && identical(_active, buffer)) {
        // 会话已关闭类失败（404/not-found）属预期态：补写暂存 chunk 后静默、
        // 不置错误条（对齐 web hydration catch 先 isExpectedClosedSessionError
        // 判定即静默 return）；其余失败可重试：错误上屏并附「重试」入口。
        if (!_isClosedSessionError(error)) {
          _setPanelError(
            '加载终端历史失败：$error',
            actionLabel: '重试',
            action: () => unawaited(_beginHistoryHydration(buffer)),
          );
        }
      }
    } finally {
      buffer.hydrating = false;
    }
  }

  /// 把 hydration 在途暂存的 live chunk 按到达顺序补写进 buffer.terminal 并清空暂存。
  ///
  /// Code Logic：[afterSeq]/[snapshotOwner] 提供时（快照成功路径）按 web
  /// appendHeldLiveAfterReplay 口径过滤——owner 与快照 owner 不一致（含一方为 null）
  /// 的 chunk 跳过、有 seq 且 seq <= 快照 lastSeq 的 chunk 跳过（内容已在权威快照内）；
  /// 无 seq 的 chunk 无法判定新旧，按序补写（fail-open 防丢失）。不传过滤参数
  /// （失败/会话切走路径）时暂存 chunk 全部按序补写。
  void _flushHeldLiveChunks(
    _MountedSession buffer, {
    int? afterSeq,
    String? snapshotOwner,
  }) {
    if (buffer.heldLiveChunks.isEmpty) {
      return;
    }
    final held = List<(int?, String?, String)>.from(buffer.heldLiveChunks);
    buffer.heldLiveChunks.clear();
    for (final (seq, owner, chunk) in held) {
      if (afterSeq != null || snapshotOwner != null) {
        if (owner != snapshotOwner) {
          continue;
        }
        if (seq != null && afterSeq != null && seq <= afterSeq) {
          continue;
        }
      }
      buffer.terminal.write(chunk);
    }
  }

  /// hydration 替换 buffer 后保持视口距底部的锚点（帧末执行，等新内容尺寸生效）；
  /// [extraUpLines] > 0 时在锚点基础上再上滚对应行数（首滑触发 hydration 后视口
  /// 随本次手势进入刚灌入的历史，至少 1 行，对齐 web scrollWhenHydrationParsed 的
  /// scrollTerminalBufferLines(Math.min(-1, pendingHydratedScrollLines))）。
  void _anchorViewportFromBottom(
    double distFromBottom, {
    int extraUpLines = 0,
  }) {
    WidgetsBinding.instance.addPostFrameCallback((_) {
      final scroll = _scrollController;
      if (_disposed || !scroll.hasClients) {
        return;
      }
      final max = scroll.position.maxScrollExtent;
      var extra = 0.0;
      if (extraUpLines > 0) {
        final rows = _active?.terminal.viewHeight ?? 0;
        final viewportHeight = _viewportSize?.height ?? 0;
        if (rows > 0 && viewportHeight > 0) {
          extra = extraUpLines * viewportHeight / rows;
        }
      }
      final target = (max - distFromBottom - extra).clamp(0.0, max);
      scroll.position.jumpTo(target);
    });
  }

  /// 停止 events 循环：递增代数、取消重连等待与空闲看门狗、断开当前流。
  void _stopEventsLoop() {
    _eventsGeneration += 1;
    _cancelEventsReconnectWait();
    _eventsIdleTimer?.cancel();
    _eventsClient?.close(force: true);
  }

  /// 启动当前会话的 events 循环（先停掉旧循环）。
  void _startEventsLoop() {
    final buffer = _active;
    if (buffer == null) {
      return;
    }
    _stopEventsLoop();
    final generation = _eventsGeneration;
    _eventsBackoffAttempt = 0;
    _eventsDown = false;
    unawaited(_runEventsLoop(generation, buffer));
  }

  /// 业务逻辑：events 流断开后必须自动重连，否则画面停留在旧状态且用户无从恢复。
  ///
  /// Code Logic：按代数运行；每轮建立 NDJSON 流直到 EOF/异常，随后如实显示断开状态并
  /// 固定节奏（2s）等待重连；代数变化（切会话/页面销毁）立即退出。
  Future<void> _runEventsLoop(int generation, _MountedSession buffer) async {
    while (mounted && !_disposed && generation == _eventsGeneration) {
      try {
        await _streamEventsOnce(generation, buffer);
      } catch (_) {
        // 断流按可重连处理。
      }
      if (!mounted || _disposed || generation != _eventsGeneration) {
        return;
      }
      // 测试模式不武装任何重连 Timer；进入退避等待前停掉空闲看门狗，避免等待期间误触发。
      _eventsIdleTimer?.cancel();
      if (widget.backgroundTimersDisabled) {
        return;
      }
      _eventsDown = true;
      _refreshStatus();
      final delay = Duration(
        milliseconds: reconnectDelayMs(
          _eventsBackoffAttempt,
          baseMs: kEventsReconnectBaseDelayMs,
          maxMs: kEventsReconnectMaxDelayMs,
        ),
      );
      _eventsBackoffAttempt += 1;
      final wake = Completer<void>();
      _eventsWake = wake;
      _eventsReconnectTimer = Timer(delay, () {
        if (!wake.isCompleted) {
          wake.complete();
        }
      });
      await wake.future;
      _eventsReconnectTimer = null;
      _eventsWake = null;
    }
  }

  /// 取消重连等待：同时取消 Timer 并完成 Completer，保证挂起的循环立即返回。
  void _cancelEventsReconnectWait() {
    _eventsReconnectTimer?.cancel();
    _eventsReconnectTimer = null;
    final wake = _eventsWake;
    if (wake != null && !wake.isCompleted) {
      wake.complete();
    }
    _eventsWake = null;
  }

  /// 建立一轮 events NDJSON 流，直到 EOF/异常/被打断。
  ///
  /// 重连接续帧时携带该会话最近 ownerInstanceId+afterSequence（增量续传：切回已
  /// 缓冲会话时不清屏，只补 gap），首帧到达后恢复「就绪」并重置退避；每收到一帧
  /// （含 heartbeat）重置空闲看门狗。
  Future<void> _streamEventsOnce(int generation, _MountedSession buffer) async {
    final sessionId = buffer.sessionId;
    _eventsClient?.close(force: true);
    HttpClient? client;
    late final LanHttpClient http;
    final factory = widget.eventsHttpClientFactory;
    if (factory == null) {
      client = HttpClient();
      _eventsClient = client;
      http = LanHttpClient(client: client);
    } else {
      // 测试注入：由工厂提供的客户端产出行流，页面不接管其生命周期。
      _eventsClient = null;
      http = factory();
    }
    var query = 'terminalSessionId=${Uri.encodeQueryComponent(sessionId)}';
    final owner = buffer.owner;
    if (owner != null) {
      query +=
          '&afterOwnerInstanceId=${Uri.encodeQueryComponent(owner)}&afterSequence=${buffer.sequence}';
    }
    var firstFrame = true;
    try {
      await for (final line in http.streamLines(
        widget.book.active!.baseUrl,
        '${TerminalController.eventsPath}?$query',
      )) {
        if (!mounted || _disposed || generation != _eventsGeneration) {
          return;
        }
        if (line.trim().isEmpty) {
          continue;
        }
        _resetEventsIdleWatchdog(generation);
        Map<String, dynamic> frame;
        try {
          frame = jsonDecode(line) as Map<String, dynamic>;
        } catch (_) {
          continue;
        }
        if (firstFrame) {
          // 同会话内已有过首连（_connectedOnce）说明本轮是断线/回前台后的重连成功：
          // 静默刷新一次会话列表，更新状态点与 pane 数；初始连接不刷（boot/切会话已刷）。
          final isReconnect = _connectedOnce;
          firstFrame = false;
          _connectedOnce = true;
          _eventsDown = false;
          _eventsBackoffAttempt = 0;
          _refreshStatus();
          if (isReconnect) {
            unawaited(_refreshSessions());
          }
        }
        final type = frame['type'] as String? ?? '';
        final frameOwner = frame['ownerInstanceId'] as String?;
        final seq = frame['sequence'];
        if (frameOwner != null) {
          // owner 变化说明桌面端 /resume 换了 owner：经 policy 记录后，
          // isHistoryHydrated(ownerInstanceId:) 会判定为未灌、允许重灌历史。
          buffer.policy.noteOwnerInstanceId(frameOwner);
          buffer.owner = frameOwner;
        }
        if (seq is int) {
          buffer.sequence = seq;
        }
        if (type == 'heartbeat') {
          continue;
        }
        if (type == 'gap') {
          await _handleGapFrame(generation, buffer);
          continue;
        }
        if (type == 'terminalOutput') {
          final payload = frame['payload'];
          if (payload is Map && payload['sessionId'] == sessionId) {
            final chunk = payload['chunk'] as String? ?? '';
            if (chunk.isNotEmpty) {
              if (buffer.hydrating) {
                // hydration 往返期间：暂存 (seq, owner, chunk)、不写 terminal——
                // 快照返回后按序补写，防止被「清屏 + 写快照」吞掉（seq 已被事件
                // 基线消费、服务端不会重发，见 _flushHeldLiveChunks）。
                buffer.heldLiveChunks.add((
                  seq is int ? seq : null,
                  frameOwner,
                  chunk,
                ));
              } else {
                buffer.terminal.write(chunk);
              }
            }
          }
        }
        if (type == 'terminalResync') {
          final payload = frame['payload'];
          if (payload is Map) {
            final snapshot =
                payload['snapshot'] as String? ??
                payload['data'] as String? ??
                '';
            if (snapshot.isNotEmpty) {
              buffer.terminal.write('\x1b[2J\x1b[H');
              buffer.terminal.write(snapshot);
            }
          }
        }
        if (type == 'terminalStatus') {
          _applyTerminalStatusFrame(frame['payload']);
          continue;
        }
        if (type == 'sessionUpdated') {
          _applySessionUpdatedFrame(frame['payload']);
          continue;
        }
      }
    } finally {
      client?.close(force: true);
      if (identical(_eventsClient, client)) {
        _eventsClient = null;
      }
    }
  }

  /// terminalStatus 帧：把已知会话的最新状态原地补丁进会话列表并刷新
  /// （对齐 web handleTerminalStatusEvent：按 sessionId 匹配，未知 id 忽略；
  /// 服务端不下发会话清单之外的状态，这里同样 fail-closed）。
  ///
  /// Code Logic：payload 宽容解析 sessionId/status 两个字符串字段；任一缺失或
  /// 状态未变化直接忽略；命中列表则重建该条目（其余字段原样保留），由 setState
  /// 驱动 chip 状态点、输入门控（`_currentSession` 为列表派生 getter，随之更新）。
  void _applyTerminalStatusFrame(Object? payload) {
    if (payload is! Map) {
      return;
    }
    final id = payload['sessionId'];
    final status = payload['status'];
    if (id is! String || id.isEmpty || status is! String || status.isEmpty) {
      return;
    }
    var changed = false;
    final next = <SessionSummary>[];
    for (final session in _sessionList) {
      if (session.id == id && session.status != status) {
        next.add(_sessionWithStatus(session, status));
        changed = true;
      } else {
        next.add(session);
      }
    }
    if (!changed || !mounted || _disposed) {
      return;
    }
    setState(() => _sessionList = next);
  }

  /// sessionUpdated 帧：用完整 DTO 原地替换已知会话（改名/pane 数/状态等元数据
  /// 立即生效，对齐 web applyKnownMobileSessionUpdatedEvent：未知 id 不得借事件
  /// 跨项目插入列表，直接忽略）。
  ///
  /// Code Logic：payload 经 SessionSummary 宽容解析；id 为空或不在已知列表中
  /// 忽略；命中则整体替换该条目并刷新（chip 名称/pane 数与输入门控随之更新；
  /// `_currentSession` 是列表 getter，无需单独同步引用）。
  void _applySessionUpdatedFrame(Object? payload) {
    if (payload is! Map) {
      return;
    }
    final parsed = SessionSummary.fromJson(Map<String, dynamic>.from(payload));
    if (parsed.id.isEmpty) {
      return;
    }
    final index = _sessionList.indexWhere((session) => session.id == parsed.id);
    if (index < 0 || !mounted || _disposed) {
      return;
    }
    setState(() {
      _sessionList = [..._sessionList]..[index] = parsed;
    });
  }

  /// 复制会话 DTO 并仅替换 status（terminalStatus 帧只迁移生命周期状态）。
  SessionSummary _sessionWithStatus(SessionSummary session, String status) {
    return SessionSummary(
      id: session.id,
      projectId: session.projectId,
      name: session.name,
      status: status,
      worktreeId: session.worktreeId,
      cols: session.cols,
      rows: session.rows,
      supportsPanes: session.supportsPanes,
      paneCount: session.paneCount,
    );
  }

  /// gap 帧：服务端无法从 afterSequence 增量续传时的兜底——清屏并重放快照后恢复 live。
  Future<void> _handleGapFrame(int generation, _MountedSession buffer) async {
    buffer.policy.onNdjsonLine({'type': 'gap'});
    buffer.policy.beginReplay();
    final replay = await _sessions.replay(buffer.sessionId);
    if (!mounted ||
        _disposed ||
        generation != _eventsGeneration ||
        !identical(_active, buffer)) {
      return;
    }
    final snapshot = _replaySnapshotOf(replay);
    buffer.terminal.write('\x1b[2J\x1b[H');
    if (snapshot.isNotEmpty) {
      buffer.terminal.write(snapshot);
    }
    buffer.policy.finishReplay();
  }

  /// 业务逻辑：45 秒无任何帧（含 heartbeat）视为半开连接，主动断开交由循环重连。
  void _resetEventsIdleWatchdog(int generation) {
    if (widget.backgroundTimersDisabled) {
      return;
    }
    _eventsIdleTimer?.cancel();
    _eventsIdleTimer = Timer(
      const Duration(milliseconds: kEventsIdleTimeoutMs),
      () {
        if (!mounted || _disposed || generation != _eventsGeneration) {
          return;
        }
        _eventsClient?.close(force: true);
      },
    );
  }

  /// 打开输入 WS（切会话/首启时调用）：重置退避后立即连接。
  Future<void> _openInput() async {
    _inputReconnectTimer?.cancel();
    _inputReconnectTimer = null;
    _inputBackoffAttempt = 0;
    await _connectInput();
  }

  /// 业务逻辑：输入 WS 断开后未确认输入结果未知，必须丢弃不重放；同时自动退避重建让用户继续键入。
  ///
  /// Code Logic：每次连接递增代数；无激活会话时不建立连接。打开成功后先经
  /// policy.onInputSocketOpened 进入「等待 ready 握手」（open ≠ 就绪），再发送 hello；
  /// 服务端帧统一交 policy.handleInputFrameText（ready/ack/error），未知类型再探测
  /// gap 帧交给同步策略；onDone 经 policy.onInputSocketClosed 丢弃未确认输入并区分
  /// 「是否有未确认输入被丢弃」；按 1s→2s→4s→…上限 10s 重建；代数变化后旧回调全部失效。
  Future<void> _connectInput() async {
    if (!mounted || _disposed) {
      return;
    }
    final buffer = _active;
    if (buffer == null) {
      return;
    }
    _inputReconnectTimer?.cancel();
    _inputReconnectTimer = null;
    final generation = ++_inputGeneration;
    final oldSocket = _socket;
    _socket = null;
    if (oldSocket != null) {
      unawaited(oldSocket.close());
    }
    WebSocket socket;
    try {
      socket = await widget.http.openWebSocket(
        widget.book.active!.baseUrl,
        TerminalController.inputPath,
        protocols: [TerminalController.inputSubprotocol],
      );
    } catch (_) {
      _markInputDown();
      _scheduleInputReconnect(generation);
      return;
    }
    if (!mounted || _disposed || generation != _inputGeneration) {
      unawaited(socket.close());
      return;
    }
    _socket = socket;
    _laneId = 'lane-${DateTime.now().microsecondsSinceEpoch}';
    // WS 已建立 ≠ 可发送：先进入 connecting，等服务端 ready 握手后才放行输入。
    buffer.policy.onInputSocketOpened();
    try {
      socket.add(
        jsonEncode({
          'type': 'hello',
          'clientId': 'mobile-${DateTime.now().microsecondsSinceEpoch}',
        }),
      );
    } catch (error) {
      buffer.policy.blockInputLink('终端输入连接建立失败：$error');
    }
    _inputBackoffAttempt = 0;
    _refreshStatus();
    socket.listen(
      (event) {
        if (generation != _inputGeneration || event is! String) {
          return;
        }
        final buffer = _active;
        if (buffer == null) {
          return;
        }
        final handled = buffer.policy.handleInputFrameText(event);
        if (!handled) {
          // 前向兼容：非 ready/ack/error 的帧（如 gap）交给同步策略。
          try {
            final frame = jsonDecode(event);
            if (frame is Map &&
                (frame['type'] == 'gap' || frame['kind'] == 'gap')) {
              buffer.policy.onNdjsonLine({'type': 'gap'});
            }
          } catch (_) {}
        }
      },
      onDone: () {
        if (!mounted || _disposed || generation != _inputGeneration) {
          return;
        }
        // 未确认输入的结果未知：断线后丢弃且永不重放；是否有丢弃由
        // inputLink.droppedUnackedOnDisconnect 区分文案。
        _active?.policy.onInputSocketClosed();
        _markInputDown();
        _scheduleInputReconnect(generation);
      },
      onError: (Object error) {
        // onDone 会随后触发，统一由 onDone 处理。
      },
      cancelOnError: false,
    );
  }

  void _markInputDown() {
    _refreshStatus();
  }

  /// 按 1s→2s→4s→…上限 10s 的退避重建输入 WS。
  void _scheduleInputReconnect(int generation) {
    if (widget.backgroundTimersDisabled) {
      return;
    }
    final delay = Duration(
      milliseconds: reconnectDelayMs(
        _inputBackoffAttempt,
        baseMs: kInputReconnectBaseDelayMs,
        maxMs: kInputReconnectMaxDelayMs,
      ),
    );
    _inputBackoffAttempt += 1;
    _inputReconnectTimer?.cancel();
    _inputReconnectTimer = Timer(delay, () {
      _inputReconnectTimer = null;
      if (!mounted || _disposed || generation != _inputGeneration) {
        return;
      }
      unawaited(_connectInput());
    });
  }

  /// xterm 内建输入路径（软键盘输入 / 快捷键 / mouse 报告）经 onOutput 到达，统一走 _send。
  void _handleTerminalOutput(String data) {
    if (data.isEmpty) {
      return;
    }
    _send(data);
  }

  /// extra keys 的页面侧发送入口（对齐 web handleExtraKeyPress）：
  /// 划选操作条可见时按 Esc 语义退出划选（清选区、不发 ESC 字节），其余原样转发。
  ///
  /// Code Logic：Esc 的 payload 恒为 `0x1b`（见 kExtraKeyPayloads['esc']，与其它
  /// CSI 序列前缀不同），据此识别；拦截时复用硬件键盘路径的退出逻辑。
  void _handleExtraKeySend(String payload) {
    if (payload == '\x1b' && _selectionBarVisible) {
      _cancelSelection();
      _refreshSelectionBarState();
      return;
    }
    _send(payload);
  }

  /// 统一发送出口：xterm onOutput、输入行、extra keys、SGR wheel 都经此进入输入 WS。
  ///
  /// Code Logic：与输入行同口径的 running/流 ready 全路径门控——空帧、无会话、非
  /// running、replay 门闩未放行、输入链路未完成 ready 握手、WS 非 open、gap 待重放
  /// 期间一律丢弃不排队（对齐 web inputEnabled = sessionId && running && 流 ready &&
  /// !busy，含 SGR wheel）。数据帧必须先经 policy.sendInput 入账（背压/超限拒绝即
  /// 丢弃并把封锁原因上屏），不绕过 policy 直接 socket.add。
  void _send(String data) {
    final buffer = _active;
    if (data.isEmpty || buffer == null || !buffer.policy.replayReady) {
      return;
    }
    if (_currentSession?.status != 'running') {
      return;
    }
    final sessionId = buffer.sessionId;
    final socket = _socket;
    if (socket == null || socket.readyState != WebSocket.open) {
      return;
    }
    if (buffer.policy.sync == TerminalSync.gapReplayRequired) {
      return;
    }
    final applied = applyStickyModifier(_sticky, data);
    if (applied.consume) {
      _setSticky(null);
    }
    final seq = _seq++;
    final accepted = buffer.policy.sendInput(applied.data, id: '$seq');
    if (accepted == null) {
      // policy 拒绝（未 ready/超限/背压）：有封锁原因时上屏错误条。
      final message = buffer.policy.inputLink.message;
      if (message != null) {
        _setPanelError(message);
      }
      return;
    }
    socket.add(
      jsonEncode({
        'type': 'input',
        'laneId': _laneId,
        'sessionId': sessionId,
        'seq': seq,
        'data': applied.data,
      }),
    );
  }

  /// 业务逻辑：sticky 武装后 3 秒无后续输入应自动解除，避免误改写普通输入。
  ///
  /// Code Logic：先取消旧 Timer 与武装记录；武装时记录注入时钟并启动
  /// [kStickyTimeoutMs] Timer 到期解除；任意按键消耗时经 _send 走这里取消 Timer。
  void _setSticky(StickyModifier? value) {
    _stickyTimer?.cancel();
    _stickyTimer = null;
    _stickyHold.cancel();
    if (!mounted || _disposed) {
      _sticky = value;
      return;
    }
    setState(() => _sticky = value);
    if (value == null) {
      return;
    }
    _stickyHold.arm(DateTime.now().millisecondsSinceEpoch);
    _stickyTimer = Timer(const Duration(milliseconds: kStickyTimeoutMs), () {
      _stickyTimer = null;
      if (!mounted || _disposed) {
        return;
      }
      setState(() => _sticky = null);
      _stickyHold.cancel();
    });
  }

  /// 状态行如实反映两条通道：未首连显示连接中；事件流断开重连优先展示；其次输入断开
  /// （按是否有未确认输入被丢弃区分文案）；正常为就绪。
  void _refreshStatus() {
    String next;
    if (!_connectedOnce) {
      next = '连接中';
    } else if (_eventsDown) {
      next = '实时输出已断开，正在重连…';
    } else if (_inputLinkDown) {
      next = _inputDroppedUnacked ? '输入已断开；未确认输入已丢弃，不会自动重放' : '输入已断开，正在重连…';
    } else {
      next = '就绪';
    }
    if (!mounted || _disposed) {
      _status = next;
      return;
    }
    setState(() => _status = next);
  }

  /// 轻提示（SnackBar，2.5s 自动消失，与 web useAutoDismissedStatus 同节奏）。
  void _toast(String message) {
    if (!mounted || _disposed) {
      return;
    }
    ScaffoldMessenger.of(context)
      ..hideCurrentSnackBar()
      ..showSnackBar(
        SnackBar(
          content: Text(message),
          duration: const Duration(milliseconds: 2500),
        ),
      );
  }

  /// 业务逻辑：手机上需要把剪贴板文本写入当前会话输入行（不带回车），方便粘贴后再编辑或手动回车。
  ///
  /// Code Logic：读系统剪贴板；为空提示 SnackBar，非空经既有 _send 原样写入当前会话。
  Future<void> _pasteText() async {
    final data = await Clipboard.getData('text/plain');
    final text = data?.text;
    if (!mounted || _disposed) {
      return;
    }
    if (text == null || text.isEmpty) {
      _toast('剪贴板没有文本');
      return;
    }
    _send(text);
  }

  /// 业务逻辑：新建会话应尽量按当前可见终端区域尺寸建 PTY，避免 TUI 首屏按默认列宽绘制后错位。
  ///
  /// Code Logic：terminal 已完成首帧布局时读实测 cols/rows（clamp 20x6..65535）；
  /// 未布局则尺寸传 null（后端按默认尺寸）；失败写入面板错误条（不再一次性 SnackBar）。
  Future<void> _createSession() async {
    if (_actionBusy != null) {
      return;
    }
    setState(() => _actionBusy = 'create');
    _setPanelError(null);
    try {
      final session = await _createSessionInternal();
      await _activateSession(session);
    } catch (error) {
      _setPanelError('创建终端失败：$error');
    } finally {
      if (mounted && !_disposed) {
        setState(() => _actionBusy = null);
      }
    }
  }

  /// 创建会话的实际网络调用（boot 与手动新建共用）；按测量状态附带 initialCols/initialRows。
  Future<SessionSummary> _createSessionInternal() {
    int? cols;
    int? rows;
    final viewport = _viewportSize;
    if (_terminalMeasured &&
        viewport != null &&
        viewport.height > 0 &&
        viewport.width > 0 &&
        _lastMeasuredCols != null &&
        _lastMeasuredRows != null) {
      cols = _lastMeasuredCols;
      rows = _lastMeasuredRows;
    }
    return _sessions.create(
      widget.project.id,
      worktreeId: widget.worktreeId,
      initialCols: cols,
      initialRows: rows,
    );
  }

  /// 业务逻辑：划选操作条「复制」把 xterm 选区写入手机剪贴板，不写 PTY；复制后退出划选。
  Future<void> _copySelection() async {
    final selection = _view.selection;
    if (selection == null) {
      return;
    }
    final text = _active?.terminal.buffer.getText(selection) ?? '';
    if (text.isEmpty) {
      return;
    }
    await Clipboard.setData(ClipboardData(text: text));
    if (mounted && !_disposed) {
      _toast('已复制');
      _view.clearSelection();
    }
  }

  /// 业务逻辑：划选操作条「取消」清除 xterm 选区并收起操作条（对齐 web exitSelecting）。
  void _cancelSelection() {
    _view.clearSelection();
  }

  /// 业务逻辑：划选操作条跟随 xterm 选区出现/消失。
  ///
  /// Code Logic：监听 xterm TerminalController（选区变化即 notifyListeners），重算
  /// 「已选 N 行」；并在终端 buffer 变化时同步刷新（选区锚点随 buffer 裁剪会静默
  /// 脱离，selection getter 为惰性求值，需主动读取才能发现）。
  void _onViewSelectionChanged() {
    _refreshSelectionBarState();
  }

  /// 依据当前选区重算操作条可见性与行数；状态无变化时不触发重建。
  void _refreshSelectionBarState() {
    if (_disposed || !mounted) {
      return;
    }
    final selection = _view.selection;
    final visible = selection != null;
    final count = visible ? _selectionLineCount(selection) : 0;
    if (visible != _selectionBarVisible || count != _selectedLineCount) {
      setState(() {
        _selectionBarVisible = visible;
        _selectedLineCount = count;
      });
    }
  }

  /// 选区覆盖的行数（begin/end 无序，取跨度的绝对值 + 1）。
  int _selectionLineCount(BufferRange range) {
    return (range.end.y - range.begin.y).abs() + 1;
  }

  /// 硬件键盘 Esc 退出划选（对齐 web Escape → exitSelecting）。
  ///
  /// Code Logic：以 [_selectionBarVisible] 为准而非惰性读取 `_view.selection`——
  /// xterm 选区锚点可能随 buffer 滚动/裁剪静默脱离（getter 届时才求值为 null），
  /// 若只读 getter 会漏判「操作条仍显示但锚点已脱离」的中间态。
  KeyEventResult _handleTerminalKeyEvent(FocusNode node, KeyEvent event) {
    if (event is KeyDownEvent &&
        event.logicalKey == LogicalKeyboardKey.escape &&
        _selectionBarVisible) {
      _view.clearSelection();
      _refreshSelectionBarState();
      return KeyEventResult.handled;
    }
    return KeyEventResult.ignored;
  }

  /// 业务逻辑：收藏 Prompt 快捷输入需要从收藏中挑一条写入当前输入行（不回车）；
  /// 筛选条件（标签/搜索）跨打开保留，由页面 state 承载。
  ///
  /// Code Logic：弹出底部收藏 sheet；选中条目经既有 _send 写入（不拼 \r）后关闭。
  Future<void> _pickFavorite() async {
    await showModalBottomSheet<void>(
      context: context,
      isScrollControlled: true,
      shape: const RoundedRectangleBorder(
        borderRadius: BorderRadius.vertical(top: Radius.circular(16)),
      ),
      builder: (sheetContext) => _FavoritePromptSheet(
        client: _prompts,
        initialTag: _favoriteTag,
        initialQuery: _favoriteQuery,
        onFiltersChanged: (tag, query) {
          _favoriteTag = tag;
          _favoriteQuery = query;
        },
        onSelect: (content) {
          _send(content);
          Navigator.of(sheetContext).pop();
        },
      ),
    );
  }

  /// 业务逻辑：Prompt 优化要把原始 Prompt 交给本机 Claude Code 优化并流式写入当前终端；
  /// 无 session / 无 worktree 的提示文案对齐 web（先选择或创建终端窗口 / 先选择 worktree）。
  ///
  /// Code Logic：弹输入对话框（提交中防重复）；workingDirectory 优先 worktree.path
  /// （对齐 web MobilePromptOptimizerSheet），为空回退 project.path；成功后对话框保留
  /// 并显示「已开始写入当前终端」，2.5s 自动消隐后关闭（对齐 web
  /// useAutoDismissedStatus 节奏）；失败在对话框内展示可读错误。
  Future<void> _optimize() async {
    final sessionId = _sessionId;
    if (sessionId == null) {
      _toast('先选择或创建终端窗口');
      return;
    }
    if (widget.worktreeId == null) {
      _toast('先选择 worktree');
      return;
    }
    if (_optimizing) {
      return;
    }
    _optimizing = true;
    try {
      await showDialog<void>(
        context: context,
        builder: (dialogContext) => _PromptOptimizerDialog(
          client: _prompts,
          sessionId: sessionId,
          workingDirectory: (widget.worktreePath?.isNotEmpty ?? false)
              ? widget.worktreePath
              : widget.project.path,
        ),
      );
    } finally {
      _optimizing = false;
    }
  }

  /// 业务逻辑：终端内一键提交（对齐 web FAB commit：message=null 让后端 AI 生成，
  /// 无输入框）；确认互锁/确认框语义不变（合并仍有确认框）。
  ///
  /// Code Logic：入口点击即发 → GitMutationTracker 稳定 clientOperationId
  /// （unknown/reconciling 复用同 id）+ GitClient.commit(message: null)；
  /// succeeded → SnackBar「提交成功」并回调 onWorktreesMutated；failedHook → hook 修复卡；
  /// unknown/传输异常 → 同 id 对账；确定失败解锁提示。
  Future<void> _commitFromToolbar() async {
    if (widget.worktreeId == null) {
      _toast('先选择 worktree');
      return;
    }
    if (_actionBusy != null) {
      return;
    }
    await _commitWorktree('');
  }

  /// 提交动作本体：message 空白传 null（后端 AI 生成，对齐 web message=null 语义）。
  ///
  /// Code Logic: unknown 相位重入（重试提交）改走对账，不盲重放（对齐 web executeMobileGitCommit
  /// reconcileOnly）；busy/reconciling 直接忽略；envelope unknown / 传输异常 → 共享对账通道。
  Future<void> _commitWorktree(String message) async {
    final worktreeId = widget.worktreeId;
    if (worktreeId == null) {
      _toast('先选择 worktree');
      return;
    }
    if (_actionBusy != null) {
      return;
    }
    // 交叉互锁的旁路封堵：merge tracker 未决（unknown/reconciling）时不得发起
    // commit——否则 hook 修复卡的「重试 commit」入口（绕过工具行按钮禁用条件）
    // 会在 merge 未对账期间产生交叉动作（与工具行互锁同一不变量）。
    if (_mergeMutation.actionLocked) {
      return;
    }
    if (_commitMutation.actionLocked) {
      if (_commitMutation.phase == GitMutationPhase.unknown) {
        await _reconcileCommit();
      }
      return;
    }
    final operationId = _commitMutation.begin(
      kind: GitMutationKind.commit,
      worktreeId: worktreeId,
      nextOperationId: buildClientOperationId('commit'),
    );
    final trimmed = message.trim();
    setState(() {
      _actionBusy = 'commit';
      _hookRepair = null;
    });
    try {
      final envelope = await _git.commit(
        worktreeId: worktreeId,
        clientOperationId: operationId,
        message: trimmed.isEmpty ? null : trimmed,
      );
      final outcome = GitMutationOutcome.fromJson(envelope);
      switch (outcome.kind) {
        case GitMutationOutcomeKind.succeeded:
          if (!mounted || _disposed) {
            return;
          }
          _commitMutation.markIdle();
          _toast('提交成功');
          widget.onWorktreesMutated?.call();
          break;
        case GitMutationOutcomeKind.failedHook:
          if (!mounted || _disposed) {
            return;
          }
          _commitMutation.markIdle();
          setState(() {
            _hookRepair = _HookRepairState(
              failure: outcome.hookFailure ?? const HookFailureView(),
              raw:
                  envelope['hookFailure'] ??
                  envelope['hook_failure'] ??
                  const {},
              clientOperationId: outcome.clientOperationId ?? operationId,
            );
          });
          break;
        case GitMutationOutcomeKind.unknown:
          await _reconcileCommit(
            envelopeOperationId: outcome.clientOperationId ?? operationId,
          );
          break;
        case GitMutationOutcomeKind.malformed:
          _commitMutation.markIdle();
          _toast('提交失败，可以重新发起');
          break;
      }
    } catch (error) {
      _afterCommitFailure(error);
    } finally {
      if (mounted && !_disposed) {
        setState(() => _actionBusy = null);
      }
    }
  }

  /// Business Logic: 网络层异常意味着请求可能已到达也可能没到达，必须按 unknown 处理等对账；
  /// 服务器已应答的错误是确定失败，解锁后允许重新发起。
  /// Code Logic: isTransportUnknownError → markUnknown（横幅可再对账）；
  /// 其余 → markIdle + 常驻错误条携带服务端详情（对齐 web commit catch →
  /// setPanelError('提交失败： <详情>')）。
  void _afterCommitFailure(Object error) {
    if (!mounted || _disposed) {
      return;
    }
    if (isTransportUnknownError(error)) {
      _commitMutation.markUnknown();
      setState(() {});
      return;
    }
    _commitMutation.markIdle();
    _setPanelError('提交失败：$error');
  }

  /// Business Logic: commit unknown 后必须用同一 clientOperationId 查 ledger 对账，不能猜成败
  /// （对齐 web executeMobileGitCommit 的 reconcileOnly 分支：ledger 终态优先，commit 无 authority）。
  /// Code Logic: reconcileWorktreeMutation（共享通道）→ 成功 → 成功流程 + onWorktreesMutated；
  /// 失败 → 显示失败原因并解锁；仍 unknown → 保持横幅可再对账。
  Future<void> _reconcileCommit({String? envelopeOperationId}) async {
    final operationId = envelopeOperationId ?? _commitMutation.operationId;
    if (operationId == null) {
      return;
    }
    _commitMutation.beginReconcile();
    if (mounted && !_disposed) {
      setState(() {});
    }
    final result = await reconcileWorktreeMutation(
      client: _git,
      projectId: widget.project.id,
      operationId: operationId,
    );
    _commitMutation.settleReconcile(result);
    if (!mounted || _disposed) {
      return;
    }
    if (result == GitMutationReconcile.confirmedSucceeded) {
      _toast('提交成功');
      widget.onWorktreesMutated?.call();
    } else if (result == GitMutationReconcile.confirmedFailed) {
      _toast('提交失败，可以重新发起');
    } else {
      setState(() {});
    }
  }

  /// 业务逻辑：failedHook 后需要与桌面相同的「让 AI 修复」出口；成功后聚焦修复终端会话。
  ///
  /// Code Logic：GitClient.repairHookFailure（原始 hookFailure 载荷原样回传）；返回
  /// terminalSessionId 时更新卡片并尝试切到该会话（先查本地列表，缺失则刷新后再查；
  /// 仍无则提示去 Git 页）。
  Future<void> _repairHook() async {
    final worktreeId = widget.worktreeId;
    final repair = _hookRepair;
    if (worktreeId == null || repair == null || _actionBusy != null) {
      return;
    }
    setState(() => _actionBusy = 'repair');
    try {
      final result = await _git.repairHookFailure(
        worktreeId: worktreeId,
        hookFailure: Map<String, dynamic>.from(repair.raw as Map),
      );
      if (!mounted || _disposed) {
        return;
      }
      final terminalSessionId =
          result['terminalSessionId'] as String? ??
          result['terminal_session_id'] as String?;
      setState(() {
        _hookRepair = _HookRepairState(
          failure: repair.failure,
          raw: repair.raw,
          clientOperationId: repair.clientOperationId,
          terminalSessionId: terminalSessionId,
        );
      });
      if (terminalSessionId == null || terminalSessionId.isEmpty) {
        _toast('修复已在 Git 页启动，请前往 Git 页查看进度');
        return;
      }
      var target = _sessionById(terminalSessionId);
      if (target == null) {
        await _refreshSessions();
        target = _sessionById(terminalSessionId);
      }
      if (target != null) {
        await _activateSession(target);
      } else {
        _toast('修复已在新的终端里运行，请前往 Git 页或会话列表查看');
      }
    } catch (_) {
      _toast('启动 AI 修复失败，请稍后重试');
    } finally {
      if (mounted && !_disposed) {
        setState(() => _actionBusy = null);
      }
    }
  }

  /// 按会话 id 在本地列表中查找 DTO。
  SessionSummary? _sessionById(String id) {
    for (final session in _sessionList) {
      if (session.id == id) {
        return session;
      }
    }
    return null;
  }

  /// 业务逻辑：修复启动后从终端重试提交。
  void _retryCommitAfterRepair() {
    setState(() => _hookRepair = null);
    unawaited(_commitWorktree(''));
  }

  /// 业务逻辑：终端内一键合并（功能分支 → 主工作区）；门控对齐 web canShowMobileTerminalMergeFab：
  /// 非主 worktree 可合并，主工作区仅在可收集分支或当前分支≠homeBranch 时开放。
  ///
  /// Code Logic：确认对话框（文案对齐 web mergeConfirm/mergeCollectConfirm，用 worktreeInfo
  /// 显示名）→ tracker 稳定 id + GitClient.merge → succeeded → SnackBar「合并成功」+
  /// onWorktreesMutated + 刷新会话（merge 会关闭源分支会话，当前会话消失时按优先级切下一个）；
  /// unknown/传输异常走同 id 对账；确定失败解锁提示。
  Future<void> _mergeWorktree() async {
    final worktreeId = widget.worktreeId;
    if (worktreeId == null) {
      _toast('先选择 worktree');
      return;
    }
    if (!canShowTerminalMergeFab(widget.worktreeInfo)) {
      return;
    }
    if (_actionBusy != null) {
      return;
    }
    if (_mergeMutation.actionLocked) {
      if (_mergeMutation.phase == GitMutationPhase.unknown) {
        await _reconcileMerge();
      }
      return;
    }
    // 合并激活 worktree 会删除源树，Files 未保存草稿可能随之成为孤儿：合并确认框
    // 前先过壳层注入的 dirty 预检（对齐 web runMobileWorktreeMergeFlow 的
    // confirmActiveWorktreeChange），取消则中止且不调后端；丢弃路径由壳层清 dirty
    // 快照，合并成功回落主树后不会对已删 worktree 再弹「请保存或丢弃」。
    final guard = widget.confirmLeaveDirty;
    if (guard != null && !await guard(worktreeId)) {
      return;
    }
    if (!mounted || _disposed) {
      return;
    }
    final confirmTree =
        widget.worktreeInfo ??
        <String, dynamic>{'id': worktreeId, 'name': worktreeId};
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (dialogContext) => AlertDialog(
        title: const Text('合并到主工作区'),
        content: Text(worktreeMergeConfirmText(confirmTree)),
        actions: [
          TextButton(
            onPressed: () => Navigator.of(dialogContext).pop(false),
            child: const Text('取消'),
          ),
          FilledButton(
            onPressed: () => Navigator.of(dialogContext).pop(true),
            child: const Text('合并'),
          ),
        ],
      ),
    );
    if (confirmed != true) {
      return;
    }
    final operationId = _mergeMutation.begin(
      kind: GitMutationKind.merge,
      worktreeId: worktreeId,
      nextOperationId: buildClientOperationId('merge'),
    );
    setState(() => _actionBusy = 'merge');
    // 合并会删除源 worktree：发起即通知壳层置忙，禁止在途期间切换 worktree
    // （对齐 web handleMergeWorktree 的 beginWorktreeOperation 全程持锁；
    // finally 成对释放，成功/失败/unknown 一律释放）。
    widget.onWorktreeOperationBusyChanged?.call(true);
    try {
      final envelope = await _git.merge(
        projectId: widget.project.id,
        worktreeId: worktreeId,
        clientOperationId: operationId,
      );
      final outcome = GitMutationOutcome.fromJson(envelope);
      if (outcome.kind == GitMutationOutcomeKind.succeeded) {
        _mergeMutation.markIdle();
        await _afterMergeSuccess();
      } else if (outcome.kind == GitMutationOutcomeKind.unknown) {
        await _reconcileMerge(
          envelopeOperationId: outcome.clientOperationId ?? operationId,
        );
      } else if (outcome.kind == GitMutationOutcomeKind.malformed) {
        _mergeMutation.markIdle();
        _toast('合并失败，请稍后重试');
      } else {
        // failedHook 不会出现在 merge 通道；保险起见解锁并按失败处理。
        _mergeMutation.markIdle();
        _toast('合并失败，请稍后重试');
      }
    } catch (error) {
      if (!mounted || _disposed) {
        return;
      }
      if (isTransportUnknownError(error)) {
        _mergeMutation.markUnknown();
        setState(() {});
        return;
      }
      // 服务器已应答的确定失败：解锁 + 常驻错误条携带服务端详情（对齐 web
      // merge catch → setPanelError('合并失败： <详情>')），不再用 2.5s toast。
      _mergeMutation.markIdle();
      _setPanelError('合并失败：$error');
    } finally {
      // 壳层互斥的成对释放（幂等；嵌套的 _reconcileMerge 各自配对）。
      widget.onWorktreeOperationBusyChanged?.call(false);
      if (mounted && !_disposed) {
        setState(() => _actionBusy = null);
      }
    }
  }

  /// Business Logic: merge unknown 后必须用同一 clientOperationId 查 ledger 对账
  /// （对齐 web MobileTerminalPanel merge unknown 分支 + 共享对账矩阵），禁止新 id 盲重放。
  /// Code Logic: reconcileWorktreeMutation（merge intent 会取主分支提交作 authority）→
  /// 成功 → 合并成功流程；失败 → 显示原因并解锁；仍 unknown → 保持横幅可再对账。
  /// 横幅独立入口与 _mergeWorktree 内嵌调用都在壳层 worktree 操作互斥锁内执行
  /// （对账期间源树可能已被删，壳层计数对嵌套 true/false 成对收敛）。
  Future<void> _reconcileMerge({String? envelopeOperationId}) async {
    final operationId = envelopeOperationId ?? _mergeMutation.operationId;
    if (operationId == null) {
      return;
    }
    _mergeMutation.beginReconcile();
    if (mounted && !_disposed) {
      setState(() {});
    }
    widget.onWorktreeOperationBusyChanged?.call(true);
    try {
      final result = await reconcileWorktreeMutation(
        client: _git,
        projectId: widget.project.id,
        operationId: operationId,
      );
      _mergeMutation.settleReconcile(result);
      if (!mounted || _disposed) {
        return;
      }
      if (result == GitMutationReconcile.confirmedSucceeded) {
        await _afterMergeSuccess();
      } else if (result == GitMutationReconcile.confirmedFailed) {
        _toast('合并失败，可以重新发起');
      } else {
        setState(() {});
      }
    } finally {
      widget.onWorktreeOperationBusyChanged?.call(false);
    }
  }

  /// 合并成功共享出口：SnackBar + 壳层刷新 worktrees + 会话善后
  /// （merge 会关闭源 worktree 会话；当前会话不在列表时按优先级切下一个）。
  Future<void> _afterMergeSuccess() async {
    _toast('合并成功');
    widget.onWorktreesMutated?.call();
    await _refreshSessions();
    if (_sessionId != null && _sessionById(_sessionId!) == null) {
      final nextSession = pickPreferredSession(_sessionList);
      if (nextSession != null) {
        await _activateSession(nextSession);
      }
    }
  }

  /// Business Logic: commit/merge 结果未知时用户必须能就地重新对账（对齐 web 面板错误区）。
  /// Code Logic: reconciling 显示「核对结果中…」；unknown 渲染 errorContainer 横幅 +
  /// 「重新对账」按钮（key 带 commit/merge 区分）；其他相位不渲染。
  Widget _buildMutationBanner(
    ThemeData theme, {
    required String label,
    required GitMutationTracker tracker,
    required Key reconcileKey,
    required Future<void> Function() onReconcile,
  }) {
    if (tracker.phase == GitMutationPhase.reconciling) {
      return Padding(
        padding: const EdgeInsets.fromLTRB(12, 0, 12, 4),
        child: Align(
          alignment: Alignment.centerLeft,
          child: Text('$label核对结果中…', style: theme.textTheme.bodySmall),
        ),
      );
    }
    if (tracker.phase != GitMutationPhase.unknown) {
      return const SizedBox.shrink();
    }
    return Container(
      margin: const EdgeInsets.fromLTRB(12, 0, 12, 4),
      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 6),
      decoration: BoxDecoration(
        color: theme.colorScheme.errorContainer,
        borderRadius: BorderRadius.circular(8),
      ),
      child: Row(
        children: [
          Expanded(
            child: Text(
              '$label操作结果未知，请刷新后人工核对',
              style: theme.textTheme.bodySmall,
            ),
          ),
          TextButton(
            key: reconcileKey,
            onPressed: () => unawaited(onReconcile()),
            child: const Text('重新对账'),
          ),
        ],
      ),
    );
  }

  /// 面板常驻错误条（对齐 web panelError：role=alert、被覆盖前常驻）；
  /// 带 action 时附「重试」类入口（如 hydration 失败重试）。
  Widget _buildPanelErrorBar(ThemeData theme) {
    final error = _panelError;
    if (error == null) {
      return const SizedBox.shrink();
    }
    final actionLabel = _panelErrorActionLabel;
    return Container(
      margin: const EdgeInsets.fromLTRB(12, 0, 12, 4),
      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 6),
      decoration: BoxDecoration(
        color: theme.colorScheme.errorContainer,
        borderRadius: BorderRadius.circular(8),
      ),
      child: Row(
        children: [
          Expanded(child: Text(error, style: theme.textTheme.bodySmall)),
          if (actionLabel != null)
            TextButton(
              key: const Key('terminal-panel-error-action'),
              onPressed: () {
                final action = _panelErrorAction;
                _setPanelError(null);
                action?.call();
              },
              child: Text(actionLabel),
            ),
        ],
      ),
    );
  }

  /// 业务逻辑：贴图要走现有 paste-image 通道，session 非 running 或输入流未就绪时禁用
  /// （统一为 running + 流 ready + 门闩放行口径，对齐 web canPasteImage）；
  /// 贴图在途（选图/上传）期间同样禁用，防重入（对齐 web pasteImageBusy）。
  bool get _canPasteImage =>
      _canUseInputActions && _actionBusy == null && !_pasteImageBusy;

  /// 相册贴图在途标志：选图→读文件→paste-image 全程置位，finally 复位。
  bool _pasteImageBusy = false;

  /// 业务逻辑：相册选图后立刻经 paste-image 通道写入（无预览）；全程 busy 防重入，
  /// HTTP 挂起期间入口禁用、完成（成败）后恢复（对齐 web pasteImageBusy +
  /// finally 复位）；失败上屏常驻错误条。
  Future<void> _pasteImage() async {
    final sessionId = _sessionId;
    if (sessionId == null || !_canPasteImage) {
      return;
    }
    if (!mounted || _disposed) {
      return;
    }
    setState(() => _pasteImageBusy = true);
    try {
      final picked = await ImagePicker().pickImage(source: ImageSource.gallery);
      if (picked == null) {
        return;
      }
      final bytes = await picked.readAsBytes();
      final b64 = base64Encode(bytes);
      final mime = picked.mimeType ?? 'image/jpeg';
      await _sessions.pasteImage(sessionId, 'data:$mime;base64,$b64');
    } catch (error) {
      _setPanelError('粘贴图片失败：$error');
    } finally {
      _pasteImageBusy = false;
      if (mounted && !_disposed) {
        setState(() {});
      }
    }
  }

  /// 进入全屏：把终端列挂到根 Overlay，盖住整个壳层（对齐 web fixed 100dvh）。
  ///
  /// 只藏壳层标题栏时，终端仍留在页面流里，手机上看起来几乎没变大。
  void _enterFullscreen() {
    if (_sessionId == null) {
      return;
    }
    _fullscreenPortal.show();
    setState(() => _fullscreen = true);
    widget.onFullscreenChanged?.call(true);
  }

  /// 退出全屏：卸下根 Overlay，并让壳层把标题栏和 worktree 条放回来。
  void _exitFullscreen() {
    if (_fullscreenPortal.isShowing) {
      _fullscreenPortal.hide();
    }
    setState(() => _fullscreen = false);
    widget.onFullscreenChanged?.call(false);
  }

  /// 会话状态色点：running=绿 / exited=灰 / 其他=橙。
  Color _sessionStatusColor(String status) {
    if (status == 'running') {
      return Colors.green.shade600;
    }
    if (status == 'exited') {
      return Colors.grey;
    }
    return Colors.orange;
  }

  /// 会话 chip 条：横向滚动，仅展示当前 project + worktree 作用域会话（对齐 web
  /// scopedSessions）；每会话一个状态点 + 名称（恒附 pane 数，0 也显示）+ 关闭 X，
  /// 末尾「+ 新建」。
  Widget _buildSessionChipBar() {
    return SizedBox(
      height: 40,
      child: ListView(
        scrollDirection: Axis.horizontal,
        padding: const EdgeInsets.symmetric(horizontal: 12),
        children: [
          for (final session in _scopedSessions)
            Padding(
              padding: const EdgeInsets.only(right: 8),
              child: InputChip(
                avatar: CircleAvatar(
                  radius: 4,
                  backgroundColor: _sessionStatusColor(session.status),
                ),
                label: Text(
                  '${session.displayName} · ${session.paneCount} pane',
                ),
                // 对齐 web MobileTerminalPanel：chip 选择不随通用动作 busy 整体
                // 禁用（commit/merge 在途仍可切换查看其它会话）；仅关闭 X 保持 gated。
                onPressed: () => unawaited(_activateSession(session)),
                onDeleted: _actionBusy == null
                    ? () => unawaited(_closeSession(session))
                    : null,
                deleteButtonTooltipMessage: '关闭窗口',
              ),
            ),
          Padding(
            padding: const EdgeInsets.only(right: 8),
            child: ActionChip(
              avatar: const Icon(Icons.add, size: 16),
              label: const Text('新建'),
              onPressed: _actionBusy == null
                  ? () => unawaited(_createSession())
                  : null,
            ),
          ),
        ],
      ),
    );
  }

  /// 窗格菜单：加窗格（split down）/ 切换窗格 / 关闭窗格，门控对齐 web。
  Widget _buildPaneMenuButton() {
    return PopupMenuButton<String>(
      icon: const Icon(Icons.splitscreen),
      tooltip: '窗格',
      onSelected: (value) {
        if (value == 'add') {
          unawaited(_createPane());
        } else if (value == 'switch') {
          unawaited(_switchPane());
        } else if (value == 'close') {
          unawaited(_closePane());
        }
      },
      itemBuilder: (context) => [
        PopupMenuItem(
          value: 'add',
          enabled: _canRunPaneMutation,
          child: const Text('新增窗格'),
        ),
        PopupMenuItem(
          value: 'switch',
          enabled: _canSwitchPane,
          child: const Text('切换窗格'),
        ),
        PopupMenuItem(
          value: 'close',
          enabled: _canRunPaneMutation,
          child: const Text('关闭窗格'),
        ),
      ],
    );
  }

  /// 终端表面：手势层（SGR 滚轮转发）+ xterm 长按划选（选区变化驱动底部操作条）+
  /// 尺寸测量 + 用户拖动取消回前台 pin + 硬件键盘 Esc 退出划选。
  Widget _buildTerminalSurface() {
    final buffer = _active;
    if (buffer == null) {
      return const SizedBox.shrink();
    }
    return LayoutBuilder(
      builder: (context, constraints) {
        _scheduleMeasure(constraints.biggest);
        return ScrollConfiguration(
          behavior: _TerminalScrollBehavior(_forwardWheel),
          child: NotificationListener<ScrollUpdateNotification>(
            onNotification: (notification) {
              // 用户拖动滚动 = 主动回看历史：取消回前台跟随 pin（对齐 web）。
              if (notification.dragDetails != null) {
                _resumePinUntil = null;
              }
              return false;
            },
            child: Listener(
              onPointerDown: _onSurfacePointerDown,
              onPointerMove: _onSurfacePointerMove,
              onPointerUp: _onSurfacePointerUp,
              onPointerCancel: _onSurfacePointerUp,
              child: TerminalView(
                key: const Key('terminal-view'),
                buffer.terminal,
                controller: _view,
                scrollController: _scrollController,
                onKeyEvent: _handleTerminalKeyEvent,
              ),
            ),
          ),
        );
      },
    );
  }

  /// hook 失败修复卡（含展开输出与 AI 修复 / 重试 / 忽略出口）。
  Widget _buildHookRepairCard(ThemeData theme) {
    final repair = _hookRepair;
    if (repair == null) {
      return const SizedBox.shrink();
    }
    final isPush = repair.failure.isPush;
    final output = repair.failure.formattedOutput;
    return Container(
      margin: const EdgeInsets.fromLTRB(12, 0, 12, 4),
      padding: const EdgeInsets.all(12),
      decoration: BoxDecoration(
        color: isPush
            ? theme.colorScheme.errorContainer
            : theme.colorScheme.surfaceContainerHighest,
        borderRadius: BorderRadius.circular(8),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Expanded(
                child: Text(
                  isPush ? 'pre-push 钩子阻止 push' : 'pre-commit 钩子阻止 commit',
                  style: theme.textTheme.titleSmall,
                ),
              ),
              if (repair.failure.exitCode != null)
                Text(
                  '退出码 ${repair.failure.exitCode}',
                  style: theme.textTheme.bodySmall,
                ),
            ],
          ),
          if (repair.terminalSessionId != null)
            Padding(
              padding: const EdgeInsets.only(top: 4),
              child: Text(
                '修复已在新的终端 tab 里运行，切换过去查看进度。',
                style: theme.textTheme.bodySmall,
              ),
            ),
          const SizedBox(height: 4),
          _HookOutputToggle(output: output),
          const SizedBox(height: 8),
          Wrap(
            spacing: 8,
            children: [
              if (repair.terminalSessionId != null)
                FilledButton(
                  onPressed: _actionBusy == null
                      ? _retryCommitAfterRepair
                      : null,
                  child: const Text('重试 commit'),
                )
              else
                FilledButton(
                  onPressed: _actionBusy == null
                      ? () => unawaited(_repairHook())
                      : null,
                  child: Text(
                    _actionBusy == 'repair' ? '正在启动 AI 修复…' : '让 AI 修复',
                  ),
                ),
              TextButton(
                onPressed: () => setState(() => _hookRepair = null),
                child: const Text('忽略'),
              ),
            ],
          ),
        ],
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    if (_error != null) {
      // boot 失败错误页：保留错误文案并给出重试入口（重试复用可重入的 _boot）。
      return Center(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Padding(
              padding: const EdgeInsets.symmetric(horizontal: 24),
              child: Text(_error!, textAlign: TextAlign.center),
            ),
            const SizedBox(height: 12),
            FilledButton.icon(
              key: const Key('terminal-boot-retry'),
              onPressed: () => unawaited(_boot()),
              icon: const Icon(Icons.refresh),
              label: const Text('重试'),
            ),
          ],
        ),
      );
    }
    final theme = Theme.of(context);
    final fullscreen = _fullscreen && _sessionId != null;
    final canUseGitActions = widget.worktreeId != null && _actionBusy == null;
    // 合并入口门控（对齐 web canShowMobileTerminalMergeFab）：非主 worktree 可合并，
    // 主工作区仅可收集分支或分支≠homeBranch 时开放。
    final mergeAllowed = canShowTerminalMergeFab(widget.worktreeInfo);
    // 交叉互锁（对齐 web canCommitWorktree/canMergeWorktree 的双向 tracker 检查）：
    // commit 与 merge 各查各的 tracker 不够——任一 tracker 处于 busy/reconciling/unknown
    // 未决相位时两个动作都要禁用，防止 commit 未对账时又发起 merge（反之亦然）产生
    // 交叉覆盖；busy 在途期 _actionBusy 本就各自禁用，关键是 unknown/reconciling
    // 落定后（_actionBusy 已清空）仍要互相锁定。
    final anyMutationPending =
        _commitMutation.actionLocked || _mergeMutation.actionLocked;
    final commitEnabled = canUseGitActions && !anyMutationPending;
    final mergeEnabled =
        canUseGitActions && mergeAllowed && !anyMutationPending;
    // 输入动作（收藏/优化/贴图/粘贴文本）统一口径：running + 流 ready + 门闩放行。
    final inputActionsEnabled = _canUseInputActions;
    // 全屏时壳层已经拿掉标题栏、状态行和 worktree 条；这里再拿掉会话 chip。
    // 动作入口仍留在工具行（对齐 web paneActions 恒可见），但全屏按钮钉在滚动区外。
    final toolbarActions = <Widget>[
      _buildPaneMenuButton(),
      IconButton(
        tooltip: '提交',
        onPressed: commitEnabled ? () => unawaited(_commitFromToolbar()) : null,
        icon: const Icon(Icons.commit),
      ),
      IconButton(
        tooltip: mergeAllowed ? '合并' : '主工作区默认分支，无需合并',
        onPressed: mergeEnabled ? () => unawaited(_mergeWorktree()) : null,
        icon: const Icon(Icons.merge_type),
      ),
      IconButton(
        tooltip: '粘贴文本',
        onPressed: inputActionsEnabled ? _pasteText : null,
        icon: const Icon(Icons.content_paste),
      ),
      IconButton(
        tooltip: '收藏 Prompt',
        onPressed: inputActionsEnabled ? _pickFavorite : null,
        icon: const Icon(Icons.star_outline),
      ),
      IconButton(
        tooltip: 'Prompt 优化',
        onPressed: inputActionsEnabled ? _optimize : null,
        icon: const Icon(Icons.auto_fix_high),
      ),
      IconButton(
        tooltip: '相册贴图',
        onPressed: _canPasteImage ? () => unawaited(_pasteImage()) : null,
        icon: const Icon(Icons.photo_outlined),
      ),
    ];
    // 全屏按钮必须钉在滚动区外面。它原先是横滑 Row 的最后一项，
    // 手指稍一移动就被滚动手势吃掉，点下去壳层不隐藏，终端看起来完全没变。
    final fullscreenButton = fullscreen
        ? IconButton(
            tooltip: '退出全屏',
            onPressed: _exitFullscreen,
            icon: const Icon(Icons.fullscreen_exit),
          )
        : IconButton(
            tooltip: '全屏',
            onPressed: _sessionId != null ? _enterFullscreen : null,
            icon: const Icon(Icons.fullscreen),
          );
    final terminalColumn = Column(
      children: [
        Padding(
          padding: EdgeInsets.symmetric(
            horizontal: 4,
            vertical: fullscreen ? 0 : 4,
          ),
          child: Row(
            children: [
              if (!fullscreen)
                Expanded(
                  child: Text(
                    _status,
                    style: theme.textTheme.bodySmall,
                    overflow: TextOverflow.ellipsis,
                  ),
                ),
              Flexible(
                // 动作区横向可滚：窄屏全屏时入口也不溢出（FAB 语义由工具行承担）。
                child: SingleChildScrollView(
                  scrollDirection: Axis.horizontal,
                  reverse: true,
                  child: Row(children: toolbarActions),
                ),
              ),
              fullscreenButton,
            ],
          ),
        ),
        if (_hookRepair != null) _buildHookRepairCard(theme),
        _buildMutationBanner(
          theme,
          label: '提交',
          tracker: _commitMutation,
          reconcileKey: const Key('terminal-commit-reconcile'),
          onReconcile: () => _reconcileCommit(),
        ),
        _buildMutationBanner(
          theme,
          label: '合并',
          tracker: _mergeMutation,
          reconcileKey: const Key('terminal-merge-reconcile'),
          onReconcile: () => _reconcileMerge(),
        ),
        _buildPanelErrorBar(theme),
        if (!fullscreen) _buildSessionChipBar(),
        Expanded(
          child: _active == null
              ? _buildEmptySessionState(theme)
              : _buildTerminalSurface(),
        ),
        if (_selectionBarVisible) _buildSelectionBar(),
        if (_active != null)
          ExtraKeysBar(
            onSend: _handleExtraKeySend,
            sticky: _sticky,
            onSticky: _setSticky,
            disabled: !_inputSendEnabled,
          ),
        // 全屏时收起底部输入框，把高度让给终端画面。输入改由点终端唤起键盘，
        // 控制键仍走上面的快捷键条；退出全屏后输入框回来。
        if (!fullscreen)
          Padding(
            padding: const EdgeInsets.all(8),
            child: Row(
              children: [
                Expanded(
                  child: TextField(
                    key: const Key('terminal-input-field'),
                    controller: _input,
                    // replay 门闩未放行、会话非 running、输入链路未完成 ready 握手
                    // （connecting/blocked/closed）或 WS 断开时禁用；任一条件恢复后
                    // 经既有 setState 路径自动恢复可用。
                    enabled: _inputSendEnabled,
                    decoration: const InputDecoration(hintText: '输入后回车发送'),
                    onSubmitted: (value) {
                      if (value.isEmpty) {
                        // 空输入不发送（不向 PTY 发裸回车）。
                        return;
                      }
                      _send('$value\r');
                      _input.clear();
                    },
                  ),
                ),
                IconButton(
                  key: const Key('terminal-input-send'),
                  onPressed: _inputSendEnabled
                      ? () {
                          final text = _input.text;
                          if (text.isEmpty) {
                            // 空输入不发送。
                            return;
                          }
                          _send('$text\r');
                          _input.clear();
                        }
                      : null,
                  icon: const Icon(Icons.send),
                ),
              ],
            ),
          ),
      ],
    );
    // 全屏时终端列只出现在根 Overlay 里，页面本身留空，避免同一个 xterm 挂两次。
    return OverlayPortal(
      controller: _fullscreenPortal,
      overlayLocation: OverlayChildLocation.rootOverlay,
      overlayChildBuilder: (overlayContext) {
        return Material(
          key: const Key('terminal-fullscreen-overlay'),
          color: Theme.of(overlayContext).colorScheme.surface,
          child: Padding(
            padding: MediaQuery.viewInsetsOf(overlayContext),
            child: SafeArea(child: terminalColumn),
          ),
        );
      },
      child: _fullscreen && _sessionId != null
          ? const SizedBox.shrink()
          : terminalColumn,
    );
  }

  /// 空态：当前 worktree 还没有终端窗口 + 手动「新建」入口
  /// （对齐 web：无会话不再自动创建，改为空态引导）。
  Widget _buildEmptySessionState(ThemeData theme) {
    return Center(
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          Text('当前 worktree 还没有终端窗口', style: theme.textTheme.bodyMedium),
          const SizedBox(height: 12),
          FilledButton.icon(
            key: const Key('terminal-empty-create'),
            onPressed: _actionBusy == null
                ? () => unawaited(_createSession())
                : null,
            icon: const Icon(Icons.add),
            label: const Text('新建'),
          ),
        ],
      ),
    );
  }

  /// 划选操作条：已选 N 行 · 复制 · 取消（对齐 web MobileTerminalPanel selection bar）。
  Widget _buildSelectionBar() {
    final enabled = _selectedLineCount > 0;
    return Container(
      key: const Key('terminal-selection-bar'),
      margin: const EdgeInsets.fromLTRB(12, 0, 12, 4),
      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 4),
      decoration: BoxDecoration(
        color: Theme.of(context).colorScheme.surfaceContainerHighest,
        borderRadius: BorderRadius.circular(8),
      ),
      child: Row(
        children: [
          Expanded(
            child: Text(
              '已选 $_selectedLineCount 行',
              style: Theme.of(context).textTheme.bodySmall,
            ),
          ),
          TextButton(
            onPressed: enabled ? () => unawaited(_copySelection()) : null,
            child: const Text('复制'),
          ),
          TextButton(onPressed: _cancelSelection, child: const Text('取消')),
        ],
      ),
    );
  }
}

/// 转发模式下禁用内建滚动（拖动经手势层编码为 SGR wheel），普通模式保持平台默认物理。
class _TerminalScrollBehavior extends ScrollBehavior {
  const _TerminalScrollBehavior(this.forwardWheel);

  final bool forwardWheel;

  @override
  ScrollPhysics getScrollPhysics(BuildContext context) {
    if (forwardWheel) {
      return const NeverScrollableScrollPhysics();
    }
    return super.getScrollPhysics(context);
  }
}

/// hook 修复卡状态（对齐 web MobileHookRepair：kind/hookFailure/clientOperationId/terminalSessionId）。
class _HookRepairState {
  const _HookRepairState({
    required this.failure,
    required this.raw,
    required this.clientOperationId,
    this.terminalSessionId,
  });

  final HookFailureView failure;

  /// 后端原始 hookFailure 载荷，修复接口原样回传。
  final Object raw;
  final String clientOperationId;
  final String? terminalSessionId;
}

/// 可展开的钩子输出（「展开钩子输出 / 收起钩子输出」，空输出给占位文案）。
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

/// 收藏 Prompt 快捷输入底部面板（对齐 web MobileFavoriteQuickInput）。
///
/// Business Logic（为什么需要）:
///   手机端用户在终端工作时，需要从收藏 Prompt 中快速挑一条插入当前会话输入行（不回车）。
///
/// Code Logic（做什么）:
///   打开即拉取收藏列表（三态：加载 / 空收藏 / 过滤后空，失败给错误 + 重试）；
///   搜索按 title/content 子串、标签按派生标签行（「全部」+ deriveTags）过滤；
///   选中条目回调 onSelect 写入终端后关闭；筛选条件变化经 onFiltersChanged 回传页面保留。
class _FavoritePromptSheet extends StatefulWidget {
  const _FavoritePromptSheet({
    required this.client,
    required this.initialTag,
    required this.initialQuery,
    required this.onFiltersChanged,
    required this.onSelect,
  });

  final PromptsClient client;
  final String initialTag;
  final String initialQuery;
  final void Function(String tag, String query) onFiltersChanged;
  final ValueChanged<String> onSelect;

  @override
  State<_FavoritePromptSheet> createState() => _FavoritePromptSheetState();
}

class _FavoritePromptSheetState extends State<_FavoritePromptSheet> {
  List<FavoritePrompt> _prompts = const [];
  bool _loading = true;
  String? _error;
  late String _selectedTag = widget.initialTag;
  late final TextEditingController _search = TextEditingController(
    text: widget.initialQuery,
  );

  @override
  void initState() {
    super.initState();
    unawaited(_load());
  }

  @override
  void dispose() {
    _search.dispose();
    super.dispose();
  }

  /// 拉取收藏列表；失败保留空列表 + 错误文案，可重试。
  Future<void> _load() async {
    setState(() {
      _loading = true;
      _error = null;
    });
    try {
      final prompts = await widget.client.listFavorites();
      if (!mounted) {
        return;
      }
      setState(() => _prompts = prompts);
    } catch (error) {
      if (!mounted) {
        return;
      }
      setState(() {
        _prompts = const [];
        _error = error.toString();
      });
    } finally {
      if (mounted) {
        setState(() => _loading = false);
      }
    }
  }

  /// 筛选条件变化时同步给页面（跨打开保留）。
  void _updateFilters(String? tag, String query) {
    final nextTag = tag ?? _selectedTag;
    if (tag != null) {
      setState(() => _selectedTag = tag);
    }
    widget.onFiltersChanged(nextTag, query);
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final maxHeight = MediaQuery.of(context).size.height * 0.8;
    final tags = deriveTagsFromFavoritePrompts(_prompts);
    final filtered = filterFavoritePrompts(
      _prompts,
      selectedTag: _selectedTag,
      allTagSentinel: _kFavoriteAllTag,
      query: _search.text,
    );
    return SafeArea(
      child: ConstrainedBox(
        constraints: BoxConstraints(maxHeight: maxHeight),
        child: Padding(
          padding: const EdgeInsets.fromLTRB(16, 12, 16, 16),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Row(
                children: [
                  Expanded(
                    child: Text(
                      '收藏的 Prompt',
                      style: theme.textTheme.titleMedium,
                    ),
                  ),
                  IconButton(
                    tooltip: '刷新',
                    onPressed: _loading ? null : _load,
                    icon: const Icon(Icons.refresh),
                  ),
                  IconButton(
                    tooltip: '关闭',
                    onPressed: () => Navigator.of(context).pop(),
                    icon: const Icon(Icons.close),
                  ),
                ],
              ),
              TextField(
                controller: _search,
                decoration: const InputDecoration(
                  prefixIcon: Icon(Icons.search),
                  hintText: '搜索收藏的 Prompt…',
                ),
                onChanged: (value) {
                  _updateFilters(null, value);
                  setState(() {});
                },
              ),
              if (tags.isNotEmpty)
                Padding(
                  padding: const EdgeInsets.only(top: 8),
                  child: Wrap(
                    spacing: 8,
                    runSpacing: 4,
                    children: [
                      for (final tag in [_kFavoriteAllTag, ...tags])
                        ChoiceChip(
                          label: Text(tag == _kFavoriteAllTag ? '全部' : tag),
                          selected: _selectedTag == tag,
                          onSelected: (_) {
                            _updateFilters(tag, _search.text);
                          },
                        ),
                    ],
                  ),
                ),
              if (_error != null)
                Padding(
                  padding: const EdgeInsets.only(top: 8),
                  child: Row(
                    children: [
                      Expanded(
                        child: Text(
                          '加载收藏失败：$_error',
                          style: TextStyle(color: theme.colorScheme.error),
                        ),
                      ),
                      TextButton(
                        onPressed: _loading ? null : _load,
                        child: const Text('重试'),
                      ),
                    ],
                  ),
                ),
              Flexible(
                child: Padding(
                  padding: const EdgeInsets.only(top: 8),
                  child: _buildList(theme, filtered),
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }

  /// 列表区三态 + 条目（标题 + 内容预览）。
  ///
  /// Code Logic：加载中 → 「加载中…」；收藏本身为空 → 空态引导；过滤后为空 →
  /// 「没有匹配的收藏 Prompt」；否则渲染条目，点击回调 onSelect（写入终端不回车）。
  Widget _buildList(ThemeData theme, List<FavoritePrompt> filtered) {
    if (_loading) {
      return const Padding(
        padding: EdgeInsets.symmetric(vertical: 24),
        child: Center(child: Text('加载中…')),
      );
    }
    if (_prompts.isEmpty) {
      return Padding(
        padding: const EdgeInsets.symmetric(vertical: 24),
        child: Column(
          children: [
            Text('还没有收藏的 Prompt', style: theme.textTheme.bodyMedium),
            const SizedBox(height: 4),
            Text(
              '在 Prompt 库点击星标即可收藏，这里会列出常用的指令。',
              style: theme.textTheme.bodySmall,
              textAlign: TextAlign.center,
            ),
          ],
        ),
      );
    }
    if (filtered.isEmpty) {
      return const Padding(
        padding: EdgeInsets.symmetric(vertical: 24),
        child: Center(child: Text('没有匹配的收藏 Prompt')),
      );
    }
    return ListView.builder(
      shrinkWrap: true,
      itemCount: filtered.length,
      itemBuilder: (context, index) {
        final prompt = filtered[index];
        return ListTile(
          leading: const Icon(Icons.star_outline, size: 18),
          title: Text(prompt.title),
          subtitle: Text(
            prompt.content,
            maxLines: 2,
            overflow: TextOverflow.ellipsis,
          ),
          onTap: () => widget.onSelect(prompt.content),
        );
      },
    );
  }
}

/// 提交信息输入对话框已移除：终端内提交对齐 web 一键直提（message=null 由后端 AI
/// 生成，无输入框）；合并仍保留确认对话框。

/// Prompt 优化对话框（对齐 web MobilePromptOptimizerSheet 的提交语义）。
///
/// Business Logic（为什么需要）:
///   把原始 Prompt 交给本机 Claude Code 优化并流式写入当前终端，无需离开终端视图。
///
/// Code Logic（做什么）:
///   输入原始 Prompt；提交中防重复（按钮禁用）；成功后对话框保留并显示
///   「已开始写入当前终端」（对齐 web promptPanel.sent），2.5s 自动消隐后关闭对话框；
///   失败在对话框内展示可读错误。targetLanguage 固定 'zh'。
class _PromptOptimizerDialog extends StatefulWidget {
  const _PromptOptimizerDialog({
    required this.client,
    required this.sessionId,
    required this.workingDirectory,
  });

  final PromptsClient client;
  final String sessionId;
  final String? workingDirectory;

  @override
  State<_PromptOptimizerDialog> createState() => _PromptOptimizerDialogState();
}

class _PromptOptimizerDialogState extends State<_PromptOptimizerDialog> {
  final TextEditingController _controller = TextEditingController();
  bool _submitting = false;
  String? _error;

  /// 成功提示「已开始写入当前终端」；2.5s 后经 Timer 关闭对话框（自动消隐）。
  String? _status;
  Timer? _statusTimer;

  @override
  void dispose() {
    _statusTimer?.cancel();
    _controller.dispose();
    super.dispose();
  }

  /// 提交优化请求；提交中防重复，成功清空输入并显示 2.5s 成功提示后自动关闭。
  Future<void> _submit() async {
    final prompt = _controller.text.trim();
    if (prompt.isEmpty || _submitting) {
      return;
    }
    setState(() {
      _submitting = true;
      _error = null;
      _status = null;
    });
    try {
      await widget.client.streamOptimizerToSession(
        prompt: prompt,
        sessionId: widget.sessionId,
        workingDirectory: widget.workingDirectory,
        targetLanguage: 'zh',
      );
      if (!mounted) {
        return;
      }
      setState(() {
        _controller.clear();
        _status = '已开始写入当前终端';
      });
      // 2.5s 自动消隐再关（对齐 web useAutoDismissedStatus 节奏后收起 sheet）。
      _statusTimer?.cancel();
      _statusTimer = Timer(const Duration(milliseconds: 2500), () {
        if (!mounted) {
          return;
        }
        Navigator.of(context).pop();
      });
    } catch (error) {
      if (mounted) {
        setState(() => _error = 'Prompt 优化失败：$error');
      }
    } finally {
      if (mounted) {
        setState(() => _submitting = false);
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    return AlertDialog(
      title: const Text('Prompt 优化'),
      content: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          TextField(
            controller: _controller,
            maxLines: 4,
            enabled: !_submitting,
            decoration: const InputDecoration(
              hintText: '输入需要优化并写入当前终端的 Prompt',
            ),
          ),
          if (_error != null)
            Padding(
              padding: const EdgeInsets.only(top: 8),
              child: Text(
                _error!,
                style: TextStyle(color: Theme.of(context).colorScheme.error),
              ),
            ),
          if (_status != null)
            Padding(
              padding: const EdgeInsets.only(top: 8),
              child: Text(
                _status!,
                style: Theme.of(context).textTheme.bodySmall,
              ),
            ),
        ],
      ),
      actions: [
        TextButton(
          onPressed: _submitting ? null : () => Navigator.of(context).pop(),
          child: const Text('取消'),
        ),
        FilledButton(
          onPressed: _submitting ? null : () => unawaited(_submit()),
          child: Text(_submitting ? '写入中' : '写入当前终端'),
        ),
      ],
    );
  }
}
