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
///   events 断开后指数退避（1s→…→15s）重连并携带最近 ownerInstanceId+afterSequence，
///   45 秒无任何帧主动断开重连，回前台立即重连；输入 WS 断开后 1s→…→10s 重建，
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
    @visibleForTesting this.sessionsClient,
    @visibleForTesting this.promptsClient,
    @visibleForTesting this.gitClient,
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

  /// 测试注入：覆盖默认 SessionsClient。
  final SessionsClient? sessionsClient;

  /// 测试注入：覆盖默认 PromptsClient。
  final PromptsClient? promptsClient;

  /// 测试注入：覆盖默认 GitClient。
  final GitClient? gitClient;

  /// 测试注入：关闭 events 空闲看门狗与两条通道的重连 Timer，避免 widget 测试挂起 Timer。
  final bool backgroundTimersDisabled;

  @override
  State<TerminalPage> createState() => _TerminalPageState();
}

class _TerminalPageState extends State<TerminalPage> with WidgetsBindingObserver {
  late final SessionsClient _sessions;
  late final PromptsClient _prompts;
  late final GitClient _git;
  late final TerminalController _policy;
  late final Terminal _terminal;
  final _view = xterm.TerminalController();
  final _input = TextEditingController();
  final _scrollController = ScrollController();
  WebSocket? _socket;
  HttpClient? _eventsClient;
  String? _sessionId;
  String? _error;
  String _status = '连接中';
  String? _laneId;
  int _seq = 1;
  String? _owner;
  int _sequence = 0;
  StickyModifier? _sticky;
  List<SessionSummary> _sessionList = [];
  bool _disposed = false;

  // 通用动作占用：'create' | 'create-pane' | 'switch-pane' | 'close-pane' | 'commit'
  // | 'merge' | 'repair' | 'close-<sessionId>'，防重入。
  String? _actionBusy;

  // hook 失败修复卡状态（对齐 web MobileHookRepair）。
  _HookRepairState? _hookRepair;

  // commit/merge 的 mutation 相位机：unknown 后锁定动作并要求同 id 对账
  // （对齐 web commitPhase/mergePhase + pickMobileMutationOperationId）。
  final GitMutationTracker _commitMutation = GitMutationTracker();
  final GitMutationTracker _mergeMutation = GitMutationTracker();

  // 全屏状态：隐藏 chip 条与工具行，仅保留状态文本与退出全屏入口。
  bool _fullscreen = false;

  // 收藏 Prompt 快捷输入：筛选条件跨打开保留（对齐 web 外层 state）。
  String _favoriteTag = _kFavoriteAllTag;
  String _favoriteQuery = '';

  // Prompt 优化提交中防重复。
  bool _optimizing = false;

  // 惰性历史 hydration：已灌过的会话集合 + 在途会话 + 累计向上意图（行）。
  final Set<String> _hydratedSessions = <String>{};
  String? _hydratingSession;
  int _hydrationIntent = 0;

  // 触控滚动转发（SGR wheel）：仅在 mouse tracking 已协商或 alt screen 时生效。
  bool _forwardWheel = false;
  final Set<int> _dragPointers = <int>{};
  TouchScrollState? _touchScroll;
  Size? _viewportSize;
  bool _measureScheduled = false;
  bool _terminalMeasured = false;
  (String, int, int)? _lastSentResize;
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
  bool _inputDown = false;

  // replay 输入门闩：初始进入与每次切换会话后的首次 replay 完成（成功或失败）前保持关闭，
  // 对齐 web shouldForwardMobileTerminalInput = replayReady && gate（门闩期间输入丢弃不排队）。
  bool _inputGateOpen = false;

  // sticky Ctrl/Alt：3 秒无后续输入自动解除。
  final StickyModifierHold _stickyHold = StickyModifierHold();
  Timer? _stickyTimer;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    _sessions = widget.sessionsClient ??
        SessionsClient(widget.http, widget.book.active!.baseUrl);
    _prompts = widget.promptsClient ??
        PromptsClient(widget.http, widget.book.active!.baseUrl);
    _git = widget.gitClient ?? GitClient(widget.http, widget.book.active!.baseUrl);
    _policy = TerminalController(sessionId: widget.preferredSessionId ?? '');
    _terminal = Terminal(maxLines: 5000, onOutput: _handleTerminalOutput);
    _terminal.addListener(_onTerminalStateMaybeChanged);
    _boot();
  }

  @override
  void dispose() {
    _disposed = true;
    WidgetsBinding.instance.removeObserver(this);
    if (_fullscreen) {
      // 销毁时若仍处于全屏必须通知壳层恢复，避免壳层停留在全屏布局。
      widget.onFullscreenChanged?.call(false);
    }
    _stickyTimer?.cancel();
    _resizeDebounce?.cancel();
    _eventsGeneration += 1;
    _cancelEventsReconnectWait();
    _eventsIdleTimer?.cancel();
    _inputGeneration += 1;
    _inputReconnectTimer?.cancel();
    _socket?.close();
    _eventsClient?.close(force: true);
    _terminal.removeListener(_onTerminalStateMaybeChanged);
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

  /// 业务逻辑：手机回前台后半开连接无法探测，输入通道必须立即重建、事件流立即重连一次。
  ///
  /// Code Logic：输入直接重连（重连 Timer 若在等待则被取消）；events 若在退避等待中
  /// 则立即唤醒并重置退避，若仍在连接中则强制断开交给循环走重连分支；随后静默刷新
  /// 一次会话列表（后台期间状态点/pane 数可能已变化，刷新失败静默）。由回前台强制的
  /// events 重连成功后还会在首帧处再静默刷一次（与退避重连共用该钩子），两次均为幂等读。
  /// xterm 4.0.0 的 RenderTerminal 自带 stick-to-bottom（滚轮位置在底部时写输出自动钉底），
  /// 回前台未在浏览历史时视口天然跟随最新输出，无需额外 pin。
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
    // 回前台后静默刷新会话列表；刷新失败静默（_refreshSessions 自吞异常）。
    unawaited(_refreshSessions());
  }

  /// 业务逻辑：boot 是页面可重入的初始化入口（initState 首启与错误页「重试」共用）：
  /// 拉会话列表 → 选/建首选会话 → 激活；任一步失败展示错误页并保留重试入口。
  ///
  /// Code Logic：先清掉上一次的 _error（initState 首次调用时为空，跳过 setState），
  /// 随后按既有顺序 list → pickPreferredSession（缺失则 create）→ _activateSession；
  /// 异常统一落 _error，由 build 渲染错误页与重试按钮。
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
      var session = pickPreferredSession(sessions, preferredId: widget.preferredSessionId);
      session ??= await _createSessionInternal();
      await _activateSession(session);
    } catch (error) {
      if (mounted) {
        setState(() => _error = error.toString());
      }
    }
  }

  /// 业务逻辑：切换会话时旧画面属于上一个会话，必须清屏并重放快照，否则新会话输出串台。
  ///
  /// Code Logic：先停掉旧 events 循环，再 focus 远端、zoom-pane 幂等、事件基线归零
  /// （ownerInstanceId / sequence 重取）、清屏 + replay 快照，然后重建输入 WS 并重启
  /// events 循环，最后刷新列表。hydration 标记按会话记录，切会话时重置在途状态。
  Future<void> _activateSession(SessionSummary session) async {
    final nextId = session.id;
    if (nextId.isEmpty ||
        !shouldResetSessionView(currentSessionId: _sessionId, nextSessionId: nextId)) {
      // 同一会话重复点击不重复清屏重放。
      return;
    }
    _stopEventsLoop();
    try {
      await _sessions.focus(nextId);
    } catch (_) {
      // focus 失败不阻断本地切换。
    }
    // 移动端单屏只能展示一个 pane：切会话后把 tmux 分屏收成 zoom 单 pane（幂等，失败静默）。
    await _ensurePaneZoomed(session);
    if (!mounted || _disposed) {
      return;
    }
    final wasFullscreen = _fullscreen;
    setState(() {
      _sessionId = nextId;
      _owner = null;
      _sequence = 0;
      _connectedOnce = false;
      _eventsDown = false;
      _status = '连接中';
      _hydratingSession = null;
      _hydrationIntent = 0;
      _fullscreen = false;
      // 首次 replay 完成前关闭输入门闩，避免输入先于历史快照执行。
      _inputGateOpen = false;
    });
    if (wasFullscreen) {
      widget.onFullscreenChanged?.call(false);
    }
    _policy.resetForSessionSwitch();
    _terminal.write('\x1b[3J\x1b[2J\x1b[H');
    try {
      final replay = await _sessions.replay(nextId);
      if (!mounted || _disposed || _sessionId != nextId) {
        return;
      }
      final snapshot = _replaySnapshotOf(replay);
      if (snapshot.isNotEmpty) {
        _terminal.write(snapshot);
      }
      _openInputGate();
    } catch (_) {
      // 快照拉取失败时仍以实时流继续，gap 帧会触发再次重放。
      // 失败同样放行输入门闩（对齐 web replay 失败分支也置 replayReady=true）。
      _openInputGate();
    }
    await _openInput();
    _startEventsLoop();
    await _refreshSessions();
  }

  /// 业务逻辑：初始进入与切换会话后的首次 replay 完成（成功或失败）后必须放行输入，
  /// 否则输入行会永远停留在门闩禁用态（对齐 web then/catch 两个分支都置 replayReady=true）。
  ///
  /// Code Logic：页面已销毁或门闩已放行时直接返回；否则置 true 并刷新 UI，
  /// 让输入行/发送按钮/extra keys（经 _send 门控）恢复可用。
  void _openInputGate() {
    if (!mounted || _disposed || _inputGateOpen) {
      return;
    }
    setState(() => _inputGateOpen = true);
  }

  /// 输入行可用性：会话已激活、权威状态为 running、replay 门闩已放行且输入 WS 处于 ready（open）。
  ///
  /// Code Logic：四者缺一即禁用——status 取自当前会话 DTO（最近一次列表刷新），
  /// 严格等于 'running' 才启用，缺失/非 running（如 exited）一律禁用
  /// （fail-closed 对齐 web inputEnabled 的 status === 'running' 严格比较）；
  /// _sessionList 刷新与 _socket 状态翻转处都有 setState，UI 会跟随刷新。
  bool get _inputRowEnabled {
    final socket = _socket;
    return _sessionId != null &&
        _currentSession?.status == 'running' &&
        _inputGateOpen &&
        socket != null &&
        socket.readyState == WebSocket.open;
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

  /// replay 响应中提取快照文本：兼容 snapshot / data / output 三种字段名（宽容解析）。
  String _replaySnapshotOf(Map<String, dynamic> replay) {
    return replay['snapshot'] as String? ??
        replay['data'] as String? ??
        replay['output'] as String? ??
        '';
  }

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
  /// Code Logic：仅对 running 且支持 panes 的会话调用 zoom-pane；后端幂等，失败静默。
  Future<void> _ensurePaneZoomed(SessionSummary session) async {
    if (session.status != 'running' || !session.supportsPanes) {
      return;
    }
    try {
      await _sessions.zoomPane(session.id);
    } catch (_) {
      // 幂等调用，失败静默（对齐任务约定）。
    }
  }

  /// 业务逻辑：手机端新增 pane 必须由 tmux 创建真实 pane，方向固定向下（对齐 web）。
  ///
  /// Code Logic：split-pane(down) → zoom 收单 pane → 刷新列表；全部动作 busy 防重入。
  Future<void> _createPane() async {
    final session = _currentSession;
    if (session == null || !session.supportsPanes || _actionBusy != null) {
      return;
    }
    setState(() => _actionBusy = 'create-pane');
    try {
      await _sessions.splitPane(session.id, direction: 'down');
      await _ensurePaneZoomed(session);
      await _refreshSessions();
    } catch (_) {
      _toast('分屏失败，请稍后重试');
    } finally {
      if (mounted && !_disposed) {
        setState(() => _actionBusy = null);
      }
    }
  }

  /// 业务逻辑：多 pane window 中一键切到下一个 pane，避免用户手敲 tmux 快捷键。
  ///
  /// Code Logic：switch-pane → zoom 幂等；仅 paneCount > 1 时可用。
  Future<void> _switchPane() async {
    final session = _currentSession;
    if (session == null || !_canSwitchPane) {
      return;
    }
    setState(() => _actionBusy = 'switch-pane');
    try {
      await _sessions.switchPane(session.id);
      await _ensurePaneZoomed(session);
    } catch (_) {
      _toast('切换窗格失败，请稍后重试');
    } finally {
      if (mounted && !_disposed) {
        setState(() => _actionBusy = null);
      }
    }
  }

  /// 业务逻辑：关闭 pane 应映射到真实 tmux pane；关掉最后一个 pane 时窗口随之移除。
  ///
  /// Code Logic：close-pane；closedWindow=true 时本地移除该会话并按优先级选下一个
  /// （pickPreferredSession，非 exited 优先）走既有切换流程，随后刷新权威列表。
  Future<void> _closePane() async {
    final session = _currentSession;
    if (session == null || !session.supportsPanes || _actionBusy != null) {
      return;
    }
    setState(() => _actionBusy = 'close-pane');
    try {
      final result = await _sessions.closePane(session.id);
      if (result.closedWindow) {
        final next = List<SessionSummary>.from(_sessionList)
          ..removeWhere((item) => item.id == result.sessionId);
        if (!mounted || _disposed) {
          return;
        }
        setState(() => _sessionList = next);
        if (_sessionId == result.sessionId) {
          _sessionId = null;
          final nextSession = pickPreferredSession(next);
          if (nextSession != null) {
            await _activateSession(nextSession);
          } else if (mounted && !_disposed) {
            setState(() => _status = '连接中');
            _stopEventsLoop();
          }
        } else {
          await _ensurePaneZoomed(session);
        }
        await _refreshSessions();
      }
    } catch (_) {
      _toast('关闭窗格失败，请稍后重试');
    } finally {
      if (mounted && !_disposed) {
        setState(() => _actionBusy = null);
      }
    }
  }

  /// 业务逻辑：手机端需要能关闭终端窗口（当前或非当前、含 exited），释放后端 PTY。
  ///
  /// Code Logic：调 sessions/close；关当前会话 → 本地移除 → pickPreferredSession 选下一个
  /// → 既有切换流程；关其他会话 → 仅移除 chip 后刷新列表；失败 SnackBar。
  Future<void> _closeSession(SessionSummary session) async {
    if (_actionBusy != null) {
      return;
    }
    setState(() => _actionBusy = 'close-${session.id}');
    try {
      await _sessions.close(session.id);
      final next = List<SessionSummary>.from(_sessionList)
        ..removeWhere((item) => item.id == session.id);
      if (!mounted || _disposed) {
        return;
      }
      setState(() => _sessionList = next);
      if (_sessionId == session.id) {
        _sessionId = null;
        final nextSession = pickPreferredSession(next);
        if (nextSession != null) {
          await _activateSession(nextSession);
        } else if (mounted && !_disposed) {
          setState(() => _status = '连接中');
          _stopEventsLoop();
        }
      }
      await _refreshSessions();
    } catch (_) {
      _toast('关闭窗口失败，请稍后重试');
    } finally {
      if (mounted && !_disposed) {
        setState(() => _actionBusy = null);
      }
    }
  }

  /// 业务逻辑：终端区域尺寸变化必须同步远端 PTY（web ResizeObserver 同语义），否则 TUI 错位。
  ///
  /// Code Logic：LayoutBuilder 记录视口尺寸，帧末读取 xterm autoResize 后的 viewWidth/viewHeight，
  /// clamp 后与最近上报基线不同才经 80ms 防抖调 sessions/resize；首次测量成功后标记
  /// _terminalMeasured，新建会话时据此携带实测尺寸。
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
      final sessionId = _sessionId;
      final viewport = _viewportSize;
      if (viewport == null || viewport.height <= 0 || viewport.width <= 0) {
        return;
      }
      _terminalMeasured = true;
      if (sessionId == null) {
        return;
      }
      final cols = clampTerminalDimension(_terminal.viewWidth, kMinTerminalCols);
      final rows = clampTerminalDimension(_terminal.viewHeight, kMinTerminalRows);
      final last = _lastSentResize;
      if (last != null && last.$1 == sessionId && last.$2 == cols && last.$3 == rows) {
        return;
      }
      _resizeDebounce?.cancel();
      _resizeDebounce = Timer(const Duration(milliseconds: 80), () {
        if (_disposed || _sessionId != sessionId) {
          return;
        }
        _lastSentResize = (sessionId, cols, rows);
        _sessions.resize(sessionId, cols, rows).then<void>(
          (_) {},
          onError: (Object _) {},
        );
      });
    });
  }

  /// 业务逻辑：xterm buffer/mouse 状态随输出变化，滚动转发门控翻转时需要重建手势层。
  ///
  /// Code Logic：terminal notifyListeners 时重算 _forwardWheel；只有翻转才 setState，
  /// 避免高频输出触发无谓重建。
  void _onTerminalStateMaybeChanged() {
    final next = _computeForwardWheel();
    if (next != _forwardWheel && mounted && !_disposed) {
      setState(() => _forwardWheel = next);
    }
  }

  /// 滚动转发判定：mouse tracking 已协商或处于 alternate screen 时把拖动编码为 SGR wheel。
  bool _computeForwardWheel() {
    return _terminal.mouseMode != MouseMode.none || _terminal.isUsingAltBuffer;
  }

  /// 业务逻辑：转发模式下单指拖动即 TUI 滚轮；非转发模式下向上拖到顶触发历史 hydration。
  ///
  /// Code Logic：仅跟踪单指 touch；行高按视口/rows 换算；转发模式经纯函数编码
  /// `CSI < 64/65 ; col ; row M`（单次最多 8 帧）走 _send；普通模式在滚到顶且存在向上
  /// 意图时调 refreshHistory replay hydration。
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
    if (state == null) {
      return;
    }
    final lineHeight = terminalTouchLineHeight(
      _viewportSize?.height ?? 0,
      _terminal.viewHeight,
      0,
    );
    final result = updateTouchScroll(state, event.position.dy, lineHeight);
    _touchScroll = result.state;
    if (result.lines == 0) {
      return;
    }
    if (_forwardWheel) {
      final wheel = encodeSgrWheelReports(result.lines);
      if (wheel.isNotEmpty) {
        _send(wheel);
      }
      return;
    }
    // 普通滚动：向上（lines<0）拖动且视口已在顶 → 触发惰性历史 hydration。
    if (_sessionId == null || _hydratingSession != null) {
      return;
    }
    final atTop = _scrollController.hasClients && _scrollController.position.pixels <= 0.5;
    if (!atTop) {
      return;
    }
    _hydrationIntent =
        accumulateHydrationScrollIntent(_hydrationIntent, result.lines, _terminal.viewHeight);
    if (_hydrationIntent <= -1) {
      unawaited(_beginHistoryHydration(_sessionId!));
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
  /// Code Logic：单飞门闩（_hydratingSession）；记录替换前「距底部行距」作锚点；
  /// 成功后清屏（含 scrollback 擦除）+ 写入快照，按会话记录已 hydrated，帧末把视口
  /// 钉回相同底部锚点；失败不标记 hydrated（可重试），live 流继续不受影响。
  Future<void> _beginHistoryHydration(String sessionId) async {
    if (_hydratedSessions.contains(sessionId) || _hydratingSession != null) {
      return;
    }
    _hydratingSession = sessionId;
    final scroll = _scrollController;
    final distFromBottom =
        scroll.hasClients ? scroll.position.maxScrollExtent - scroll.position.pixels : null;
    try {
      final replay = await _sessions.replay(sessionId, refreshHistory: true);
      if (!mounted || _disposed || _sessionId != sessionId) {
        return;
      }
      final snapshot = _replaySnapshotOf(replay);
      _terminal.write('\x1b[3J\x1b[2J\x1b[H');
      if (snapshot.isNotEmpty) {
        _terminal.write(snapshot);
      }
      _hydratedSessions.add(sessionId);
      _hydrationIntent = 0;
      if (distFromBottom != null) {
        _anchorViewportFromBottom(distFromBottom);
      }
    } catch (_) {
      // hydration 失败可重试：不标记 hydrated，不打断 live 流。
    } finally {
      if (!_disposed && _hydratingSession == sessionId) {
        _hydratingSession = null;
      }
    }
  }

  /// hydration 替换 buffer 后保持视口距底部的锚点（帧末执行，等新内容尺寸生效）。
  void _anchorViewportFromBottom(double distFromBottom) {
    WidgetsBinding.instance.addPostFrameCallback((_) {
      final scroll = _scrollController;
      if (_disposed || !scroll.hasClients) {
        return;
      }
      final max = scroll.position.maxScrollExtent;
      final target = (max - distFromBottom).clamp(0.0, max);
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
    final sessionId = _sessionId;
    if (sessionId == null) {
      return;
    }
    _stopEventsLoop();
    final generation = _eventsGeneration;
    _eventsBackoffAttempt = 0;
    _eventsDown = false;
    unawaited(_runEventsLoop(generation, sessionId));
  }

  /// 业务逻辑：events 流断开后必须自动重连，否则画面停留在旧状态且用户无从恢复。
  ///
  /// Code Logic：按代数运行；每轮建立 NDJSON 流直到 EOF/异常，随后如实显示断开状态并
  /// 指数退避（1s→2s→4s→…上限 15s）等待重连；代数变化（切会话/页面销毁）立即退出。
  Future<void> _runEventsLoop(int generation, String sessionId) async {
    while (mounted && !_disposed && generation == _eventsGeneration) {
      try {
        await _streamEventsOnce(generation, sessionId);
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
  /// 重连接续帧时携带最近 ownerInstanceId+afterSequence（现有字段），首帧到达后
  /// 恢复「就绪」并重置退避；每收到一帧（含 heartbeat）重置 45s 空闲看门狗。
  Future<void> _streamEventsOnce(int generation, String sessionId) async {
    _eventsClient?.close(force: true);
    final client = HttpClient();
    _eventsClient = client;
    final http = LanHttpClient(client: client);
    var query = 'terminalSessionId=${Uri.encodeQueryComponent(sessionId)}';
    final owner = _owner;
    if (owner != null) {
      query +=
          '&afterOwnerInstanceId=${Uri.encodeQueryComponent(owner)}&afterSequence=$_sequence';
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
          _owner = frameOwner;
        }
        if (seq is int) {
          _sequence = seq;
        }
        if (type == 'heartbeat') {
          continue;
        }
        if (type == 'gap') {
          await _handleGapFrame(generation, sessionId);
          continue;
        }
        if (type == 'terminalOutput') {
          final payload = frame['payload'];
          if (payload is Map && payload['sessionId'] == sessionId) {
            final chunk = payload['chunk'] as String? ?? '';
            if (chunk.isNotEmpty) {
              _terminal.write(chunk);
            }
          }
        }
        if (type == 'terminalResync') {
          final payload = frame['payload'];
          if (payload is Map) {
            final snapshot =
                payload['snapshot'] as String? ?? payload['data'] as String? ?? '';
            if (snapshot.isNotEmpty) {
              _terminal.write('\x1b[2J\x1b[H');
              _terminal.write(snapshot);
            }
          }
        }
      }
    } finally {
      client.close(force: true);
      if (identical(_eventsClient, client)) {
        _eventsClient = null;
      }
    }
  }

  /// gap 帧：清屏并重放快照后才能恢复 live（沿用既有 replay 语义）。
  Future<void> _handleGapFrame(int generation, String sessionId) async {
    _policy.onNdjsonLine({'type': 'gap'});
    _policy.beginReplay();
    final replay = await _sessions.replay(sessionId);
    if (!mounted || _disposed || generation != _eventsGeneration || _sessionId != sessionId) {
      return;
    }
    final snapshot = _replaySnapshotOf(replay);
    _terminal.write('\x1b[2J\x1b[H');
    if (snapshot.isNotEmpty) {
      _terminal.write(snapshot);
    }
    _policy.finishReplay();
  }

  /// 业务逻辑：45 秒无任何帧（含 heartbeat）视为半开连接，主动断开交由循环重连。
  void _resetEventsIdleWatchdog(int generation) {
    if (widget.backgroundTimersDisabled) {
      return;
    }
    _eventsIdleTimer?.cancel();
    _eventsIdleTimer = Timer(const Duration(milliseconds: kEventsIdleTimeoutMs), () {
      if (!mounted || _disposed || generation != _eventsGeneration) {
        return;
      }
      _eventsClient?.close(force: true);
    });
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
  /// Code Logic：每次连接递增代数；打开成功后发送 hello 并重置退避；onDone 丢弃未确认输入，
  /// 按 1s→2s→4s→…上限 10s 重建；代数变化（切会话/页面销毁/更新重建）后旧连接回调全部失效。
  Future<void> _connectInput() async {
    if (!mounted || _disposed) {
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
    socket.add(jsonEncode({
      'type': 'hello',
      'clientId': 'mobile-${DateTime.now().microsecondsSinceEpoch}',
    }));
    _inputBackoffAttempt = 0;
    _inputDown = false;
    _refreshStatus();
    socket.listen((event) {
      if (generation != _inputGeneration || event is! String) {
        return;
      }
      try {
        final frame = jsonDecode(event);
        if (frame is Map && frame['type'] == 'ack') {
          _policy.onAck('${frame['seq']}');
        }
        if (frame is Map && (frame['type'] == 'gap' || frame['kind'] == 'gap')) {
          _policy.onNdjsonLine({'type': 'gap'});
        }
      } catch (_) {}
    }, onDone: () {
      if (!mounted || _disposed || generation != _inputGeneration) {
        return;
      }
      // 未确认输入的结果未知：断线后丢弃且永不重放（controller 既有策略）。
      _policy.takeUnackedOnDisconnect();
      _markInputDown();
      _scheduleInputReconnect(generation);
    }, onError: (Object error) {
      // onDone 会随后触发，统一由 onDone 处理。
    }, cancelOnError: false);
  }

  void _markInputDown() {
    _inputDown = true;
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

  /// 统一发送出口：xterm onOutput、输入行、extra keys、SGR wheel 都经此进入输入 WS。
  ///
  /// Code Logic：空帧直接丢弃；replay 门闩未放行（初始进入/切会话后首次 replay 未完成）
  /// 丢弃不排队（对齐 web shouldForwardMobileTerminalInput = replayReady && gate）；
  /// 无会话或输入 WS 非 open 时静默丢弃；gap 待重放期间沿用 controller 策略丢弃。
  void _send(String data) {
    if (data.isEmpty || !_inputGateOpen) {
      return;
    }
    final sessionId = _sessionId;
    final socket = _socket;
    if (sessionId == null || socket == null || socket.readyState != WebSocket.open) {
      return;
    }
    if (_policy.sync == TerminalSync.gapReplayRequired) {
      return;
    }
    final applied = applyStickyModifier(_sticky, data);
    if (applied.consume) {
      _setSticky(null);
    }
    final seq = _seq++;
    _policy.sendInput(applied.data);
    socket.add(jsonEncode({
      'type': 'input',
      'laneId': _laneId,
      'sessionId': sessionId,
      'seq': seq,
      'data': applied.data,
    }));
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

  /// 状态行如实反映两条通道：未首连显示连接中；事件流断开重连优先展示；其次输入断开；正常为就绪。
  void _refreshStatus() {
    String next;
    if (!_connectedOnce) {
      next = '连接中';
    } else if (_eventsDown) {
      next = '实时输出已断开，正在重连…';
    } else if (_inputDown) {
      next = '输入已断开；未确认输入不会自动重放';
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
      ..showSnackBar(SnackBar(
        content: Text(message),
        duration: const Duration(milliseconds: 2500),
      ));
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
  /// 未布局则尺寸传 null（后端按默认尺寸）。
  Future<void> _createSession() async {
    if (_actionBusy != null) {
      return;
    }
    setState(() => _actionBusy = 'create');
    try {
      final session = await _createSessionInternal();
      await _activateSession(session);
    } catch (_) {
      _toast('新建会话失败，请稍后重试');
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
    if (_terminalMeasured && viewport != null && viewport.height > 0 && viewport.width > 0) {
      cols = clampTerminalDimension(_terminal.viewWidth, kMinTerminalCols);
      rows = clampTerminalDimension(_terminal.viewHeight, kMinTerminalRows);
    }
    return _sessions.create(
      widget.project.id,
      worktreeId: widget.worktreeId,
      initialCols: cols,
      initialRows: rows,
    );
  }

  /// 业务逻辑：长按复制把 xterm 选区写入手机剪贴板，不写 PTY。
  Future<void> _copySelection() async {
    final selection = _view.selection;
    if (selection == null) {
      return;
    }
    final text = _terminal.buffer.getText(selection);
    if (text.isEmpty) {
      return;
    }
    await Clipboard.setData(ClipboardData(text: text));
    if (mounted && !_disposed) {
      _toast('已复制');
    }
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
  /// （对齐 web MobilePromptOptimizerSheet），为空回退 project.path；成功后关闭对话框、
  /// 清空输入并 SnackBar「已发送」（2.5s）；失败在对话框内展示可读错误。
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
          workingDirectory:
              (widget.worktreePath?.isNotEmpty ?? false) ? widget.worktreePath : widget.project.path,
          onSent: () {
            Navigator.of(dialogContext).pop();
            _toast('已发送');
          },
        ),
      );
    } finally {
      _optimizing = false;
    }
  }

  /// 业务逻辑：终端内一键提交（与桌面 Git 历史同口径）：留空 message 由后端 AI 生成。
  ///
  /// Code Logic：弹 message 输入对话框（可空提交）→ GitMutationTracker 稳定 clientOperationId
  /// （unknown/reconciling 复用同 id）+ GitClient.commit；succeeded → SnackBar「提交成功」并回调
  /// onWorktreesMutated；failedHook → hook 修复卡；unknown/传输异常 → 同 id 对账；确定失败解锁提示。
  Future<void> _showCommitDialog() async {
    if (widget.worktreeId == null) {
      _toast('先选择 worktree');
      return;
    }
    if (_actionBusy != null) {
      return;
    }
    final message = await showDialog<String>(
      context: context,
      builder: (dialogContext) => const _CommitMessageDialog(),
    );
    if (message == null) {
      return;
    }
    await _commitWorktree(message);
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
              raw: envelope['hookFailure'] ?? envelope['hook_failure'] ?? const {},
              clientOperationId: outcome.clientOperationId ?? operationId,
            );
          });
          break;
        case GitMutationOutcomeKind.unknown:
          await _reconcileCommit(envelopeOperationId: outcome.clientOperationId ?? operationId);
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
  /// Code Logic: isTransportUnknownError → markUnknown（横幅可再对账）；其余 → markIdle + 失败提示。
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
    _toast('提交失败，可以重新发起');
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
      final terminalSessionId = result['terminalSessionId'] as String? ??
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
    final confirmTree = widget.worktreeInfo ??
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
        await _reconcileMerge(envelopeOperationId: outcome.clientOperationId ?? operationId);
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
      _mergeMutation.markIdle();
      _toast('合并失败，请稍后重试');
    } finally {
      if (mounted && !_disposed) {
        setState(() => _actionBusy = null);
      }
    }
  }

  /// Business Logic: merge unknown 后必须用同一 clientOperationId 查 ledger 对账
  /// （对齐 web MobileTerminalPanel merge unknown 分支 + 共享对账矩阵），禁止新 id 盲重放。
  /// Code Logic: reconcileWorktreeMutation（merge intent 会取主分支提交作 authority）→
  /// 成功 → 合并成功流程；失败 → 显示原因并解锁；仍 unknown → 保持横幅可再对账。
  Future<void> _reconcileMerge({String? envelopeOperationId}) async {
    final operationId = envelopeOperationId ?? _mergeMutation.operationId;
    if (operationId == null) {
      return;
    }
    _mergeMutation.beginReconcile();
    if (mounted && !_disposed) {
      setState(() {});
    }
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
              '$label结果未知，请重新对账。',
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

  /// 业务逻辑：贴图要走现有 paste-image 通道，session 非 running 或输入流未就绪时禁用
  /// （对齐 web canPasteImage 门控）。
  bool get _canPasteImage {
    final session = _currentSession;
    return _sessionId != null &&
        session?.status == 'running' &&
        _socket != null &&
        _socket!.readyState == WebSocket.open &&
        _actionBusy == null;
  }

  Future<void> _pasteImage() async {
    final sessionId = _sessionId;
    if (sessionId == null || !_canPasteImage) {
      return;
    }
    final picked = await ImagePicker().pickImage(source: ImageSource.gallery);
    if (picked == null) {
      return;
    }
    final bytes = await picked.readAsBytes();
    final b64 = base64Encode(bytes);
    final mime = picked.mimeType ?? 'image/jpeg';
    try {
      await _sessions.pasteImage(sessionId, 'data:$mime;base64,$b64');
    } catch (_) {
      _toast('粘贴图片失败，请稍后重试');
    }
  }

  /// 进入全屏：有可见会话时隐藏 chip 条与工具行并回调壳层。
  void _enterFullscreen() {
    if (_sessionId == null) {
      return;
    }
    setState(() => _fullscreen = true);
    widget.onFullscreenChanged?.call(true);
  }

  /// 退出全屏并回调壳层。
  void _exitFullscreen() {
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

  /// 会话 chip 条：横向滚动，每会话一个状态点 + 名称（附 pane 数）+ 关闭 X，末尾「+ 新建」。
  Widget _buildSessionChipBar() {
    return SizedBox(
      height: 40,
      child: ListView(
        scrollDirection: Axis.horizontal,
        padding: const EdgeInsets.symmetric(horizontal: 12),
        children: [
          for (final session in _sessionList)
            Padding(
              padding: const EdgeInsets.only(right: 8),
              child: InputChip(
                avatar: CircleAvatar(
                  radius: 4,
                  backgroundColor: _sessionStatusColor(session.status),
                ),
                label: Text(session.paneCount > 0
                    ? '${session.displayName} · ${session.paneCount} pane'
                    : session.displayName),
                onPressed: _actionBusy == null
                    ? () => unawaited(_activateSession(session))
                    : null,
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
              onPressed:
                  _actionBusy == null ? () => unawaited(_createSession()) : null,
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

  /// 终端表面：手势层（SGR 滚轮转发）+ 长按复制 + 尺寸测量 + 滚动条控制。
  Widget _buildTerminalSurface() {
    return LayoutBuilder(
      builder: (context, constraints) {
        _scheduleMeasure(constraints.biggest);
        return ScrollConfiguration(
          behavior: _TerminalScrollBehavior(_forwardWheel),
          child: Listener(
            onPointerDown: _onSurfacePointerDown,
            onPointerMove: _onSurfacePointerMove,
            onPointerUp: _onSurfacePointerUp,
            onPointerCancel: _onSurfacePointerUp,
            child: GestureDetector(
              onLongPress: _copySelection,
              child: TerminalView(
                _terminal,
                controller: _view,
                scrollController: _scrollController,
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
        color: isPush ? theme.colorScheme.errorContainer : theme.colorScheme.surfaceContainerHighest,
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
                Text('退出码 ${repair.failure.exitCode}', style: theme.textTheme.bodySmall),
            ],
          ),
          if (repair.terminalSessionId != null)
            Padding(
              padding: const EdgeInsets.only(top: 4),
              child: Text('修复已在新的终端 tab 里运行，切换过去查看进度。',
                  style: theme.textTheme.bodySmall),
            ),
          const SizedBox(height: 4),
          _HookOutputToggle(output: output),
          const SizedBox(height: 8),
          Wrap(
            spacing: 8,
            children: [
              if (repair.terminalSessionId != null)
                FilledButton(
                  onPressed: _actionBusy == null ? _retryCommitAfterRepair : null,
                  child: const Text('重试 commit'),
                )
              else
                FilledButton(
                  onPressed:
                      _actionBusy == null ? () => unawaited(_repairHook()) : null,
                  child: Text(_actionBusy == 'repair' ? '正在启动 AI 修复…' : '让 AI 修复'),
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
              child: Text(
                _error!,
                textAlign: TextAlign.center,
              ),
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
    final mergeEnabled = canUseGitActions && mergeAllowed && !anyMutationPending;
    return Column(
      children: [
        Padding(
          padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 4),
          child: Row(
            children: [
              Expanded(
                child: Text(
                  _status,
                  style: theme.textTheme.bodySmall,
                  overflow: TextOverflow.ellipsis,
                ),
              ),
              if (fullscreen)
                IconButton(
                  tooltip: '退出全屏',
                  onPressed: _exitFullscreen,
                  icon: const Icon(Icons.fullscreen_exit),
                )
              else ...[
                _buildPaneMenuButton(),
                IconButton(
                  tooltip: '提交',
                  onPressed: commitEnabled ? () => unawaited(_showCommitDialog()) : null,
                  icon: const Icon(Icons.commit),
                ),
                IconButton(
                  tooltip: mergeAllowed ? '合并' : '主工作区默认分支，无需合并',
                  onPressed: mergeEnabled ? () => unawaited(_mergeWorktree()) : null,
                  icon: const Icon(Icons.merge_type),
                ),
                IconButton(
                  tooltip: '粘贴文本',
                  onPressed: _pasteText,
                  icon: const Icon(Icons.content_paste),
                ),
                IconButton(
                  tooltip: '收藏 Prompt',
                  onPressed: _sessionId != null ? _pickFavorite : null,
                  icon: const Icon(Icons.star_outline),
                ),
                IconButton(
                  tooltip: 'Prompt 优化',
                  onPressed: _sessionId != null ? _optimize : null,
                  icon: const Icon(Icons.auto_fix_high),
                ),
                IconButton(
                  tooltip: '相册贴图',
                  onPressed: _canPasteImage ? () => unawaited(_pasteImage()) : null,
                  icon: const Icon(Icons.photo_outlined),
                ),
                IconButton(
                  tooltip: '全屏',
                  onPressed: _sessionId != null ? _enterFullscreen : null,
                  icon: const Icon(Icons.fullscreen),
                ),
              ],
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
        if (!fullscreen) _buildSessionChipBar(),
        Expanded(
          child: _buildTerminalSurface(),
        ),
        ExtraKeysBar(
          onSend: _send,
          sticky: _sticky,
          onSticky: _setSticky,
        ),
        Padding(
          padding: const EdgeInsets.all(8),
          child: Row(
            children: [
              Expanded(
                child: TextField(
                  key: const Key('terminal-input-field'),
                  controller: _input,
                  // replay 门闩未放行、会话非 running 或输入 WS 非 ready
                  // （connecting/blocked/closed）时禁用；任一条件恢复后
                  // 经既有 setState 路径自动恢复可用。
                  enabled: _inputRowEnabled,
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
                onPressed: _inputRowEnabled
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
              style: theme.textTheme.bodySmall?.copyWith(fontFamily: 'monospace'),
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
  late final TextEditingController _search = TextEditingController(text: widget.initialQuery);

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
                  Expanded(child: Text('收藏的 Prompt', style: theme.textTheme.titleMedium)),
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

/// 提交信息输入对话框（可空提交 = 让 AI 生成 message，对齐 web mobileGitCommit message=null）。
///
/// Business Logic（为什么需要）:
///   终端内一键提交需要与桌面 Git 历史同口径：用户可以留空让后端用 Claude Code 生成
///   提交信息，也可以手写 message。
///
/// Code Logic（做什么）:
///   输入 message；「提交」返回文本（trim 后可为空串，由调用方转 null）；「取消」返回 null。
class _CommitMessageDialog extends StatefulWidget {
  const _CommitMessageDialog();

  @override
  State<_CommitMessageDialog> createState() => _CommitMessageDialogState();
}

class _CommitMessageDialogState extends State<_CommitMessageDialog> {
  final TextEditingController _controller = TextEditingController();

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return AlertDialog(
      title: const Text('提交'),
      content: TextField(
        controller: _controller,
        maxLines: 3,
        autofocus: true,
        decoration: const InputDecoration(
          hintText: '提交信息（留空由 AI 生成）',
        ),
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.of(context).pop(),
          child: const Text('取消'),
        ),
        FilledButton(
          onPressed: () => Navigator.of(context).pop(_controller.text),
          child: const Text('提交'),
        ),
      ],
    );
  }
}

/// Prompt 优化对话框（对齐 web MobilePromptOptimizerSheet 的提交语义）。
///
/// Business Logic（为什么需要）:
///   把原始 Prompt 交给本机 Claude Code 优化并流式写入当前终端，无需离开终端视图。
///
/// Code Logic（做什么）:
///   输入原始 Prompt；提交中防重复（按钮禁用）；成功经 onSent 关闭对话框并触发
///   SnackBar「已发送」；失败在对话框内展示可读错误。targetLanguage 固定 'zh'。
class _PromptOptimizerDialog extends StatefulWidget {
  const _PromptOptimizerDialog({
    required this.client,
    required this.sessionId,
    required this.workingDirectory,
    required this.onSent,
  });

  final PromptsClient client;
  final String sessionId;
  final String? workingDirectory;
  final VoidCallback onSent;

  @override
  State<_PromptOptimizerDialog> createState() => _PromptOptimizerDialogState();
}

class _PromptOptimizerDialogState extends State<_PromptOptimizerDialog> {
  final TextEditingController _controller = TextEditingController();
  bool _submitting = false;
  String? _error;

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  /// 提交优化请求；提交中防重复，成功清空输入（对话框随后关闭）并回调 onSent。
  Future<void> _submit() async {
    final prompt = _controller.text.trim();
    if (prompt.isEmpty || _submitting) {
      return;
    }
    setState(() {
      _submitting = true;
      _error = null;
    });
    try {
      await widget.client.streamOptimizerToSession(
        prompt: prompt,
        sessionId: widget.sessionId,
        workingDirectory: widget.workingDirectory,
        targetLanguage: 'zh',
      );
      _controller.clear();
      if (mounted) {
        widget.onSent();
      }
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
