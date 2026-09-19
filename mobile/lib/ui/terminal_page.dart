import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:image_picker/image_picker.dart';
import 'package:xterm/xterm.dart' hide TerminalController;
import 'package:xterm/xterm.dart' as xterm show TerminalController;

import '../address_book/book.dart';
import '../core/lan_http.dart';
import '../projects/client.dart';
import '../prompts/client.dart';
import '../sessions/client.dart';
import '../terminal/controller.dart';
import '../terminal/extra_keys.dart';
import 'extra_keys_bar.dart';

/// 局域网远端项目终端页。
///
/// Business Logic（为什么需要）:
///   手机上需要实时查看并输入远端 Workbench 终端：输出走 NDJSON events 长连接，
///   输入走独立 WebSocket；局域网环境两者都会频繁半开断连，必须自动重连才能可靠使用。
///
/// Code Logic（做什么）:
///   会话切换 = 清屏 + replay 快照 + 事件基线归零 + 重建输入 WS + 重启 events 循环；
///   events 断开后指数退避（1s→…→15s）重连并携带最近 ownerInstanceId+afterSequence，
///   45 秒无任何帧主动断开重连，回前台立即重连；输入 WS 断开后 1s→…→10s 重建，
///   未确认输入按既有策略丢弃不重放；另提供粘贴文本、收藏 Prompt、优化、相册贴图与会话 chip 条。
class TerminalPage extends StatefulWidget {
  const TerminalPage({
    super.key,
    required this.book,
    required this.http,
    required this.project,
    this.preferredSessionId,
    this.worktreeId,
  });

  final AddressBook book;
  final LanHttpClient http;
  final ProjectSummary project;
  final String? preferredSessionId;
  final String? worktreeId;

  @override
  State<TerminalPage> createState() => _TerminalPageState();
}

class _TerminalPageState extends State<TerminalPage> with WidgetsBindingObserver {
  late final SessionsClient _sessions;
  late final TerminalController _policy;
  final Terminal _terminal = Terminal(maxLines: 5000);
  final _view = xterm.TerminalController();
  final _input = TextEditingController();
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

  // sticky Ctrl/Alt：3 秒无后续输入自动解除。
  final StickyModifierHold _stickyHold = StickyModifierHold();
  Timer? _stickyTimer;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    _sessions = SessionsClient(widget.http, widget.book.active!.baseUrl);
    _policy = TerminalController(sessionId: widget.preferredSessionId ?? '');
    _boot();
  }

  @override
  void dispose() {
    _disposed = true;
    WidgetsBinding.instance.removeObserver(this);
    _stickyTimer?.cancel();
    _eventsGeneration += 1;
    _cancelEventsReconnectWait();
    _eventsIdleTimer?.cancel();
    _inputGeneration += 1;
    _inputReconnectTimer?.cancel();
    _socket?.close();
    _eventsClient?.close(force: true);
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
  /// 则立即唤醒并重置退避，若仍在连接中则强制断开交给循环走重连分支。
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
  }

  Future<void> _boot() async {
    try {
      final sessions = await _sessions.list(widget.project.id);
      if (!mounted || _disposed) {
        return;
      }
      setState(() => _sessionList = sessions);
      var session = pickPreferredSession(sessions, preferredId: widget.preferredSessionId);
      session ??= await _sessions.create(widget.project.id, worktreeId: widget.worktreeId);
      await _activateSession(session);
    } catch (error) {
      if (mounted) {
        setState(() => _error = error.toString());
      }
    }
  }

  /// 业务逻辑：切换会话时旧画面属于上一个会话，必须清屏并重放快照，否则新会话输出串台。
  ///
  /// Code Logic：先停掉旧 events 循环，再 focus 远端、事件基线归零（ownerInstanceId /
  /// sequence 重取）、清屏 + replay 快照，然后重建输入 WS 并重启 events 循环，最后刷新列表。
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
    if (!mounted || _disposed) {
      return;
    }
    setState(() {
      _sessionId = nextId;
      _owner = null;
      _sequence = 0;
      _connectedOnce = false;
      _eventsDown = false;
      _status = '连接中';
    });
    _policy.resetForSessionSwitch();
    _terminal.write('\x1b[3J\x1b[2J\x1b[H');
    try {
      final replay = await _sessions.replay(nextId);
      if (!mounted || _disposed || _sessionId != nextId) {
        return;
      }
      final snapshot = replay['snapshot'] as String? ??
          replay['data'] as String? ??
          replay['output'] as String? ??
          '';
      if (snapshot.isNotEmpty) {
        _terminal.write(snapshot);
      }
    } catch (_) {
      // 快照拉取失败时仍以实时流继续，gap 帧会触发再次重放。
    }
    await _openInput();
    _startEventsLoop();
    try {
      final sessions = await _sessions.list(widget.project.id);
      if (mounted && !_disposed) {
        setState(() => _sessionList = sessions);
      }
    } catch (_) {
      // 列表刷新失败不影响已完成的切换。
    }
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
      // 进入退避等待前停掉空闲看门狗，避免等待期间误触发。
      _eventsIdleTimer?.cancel();
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
          firstFrame = false;
          _connectedOnce = true;
          _eventsDown = false;
          _eventsBackoffAttempt = 0;
          _refreshStatus();
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
    final snapshot = replay['snapshot'] as String? ??
        replay['data'] as String? ??
        replay['output'] as String? ??
        '';
    _terminal.write('\x1b[2J\x1b[H');
    if (snapshot.isNotEmpty) {
      _terminal.write(snapshot);
    }
    _policy.finishReplay();
  }

  /// 业务逻辑：45 秒无任何帧（含 heartbeat）视为半开连接，主动断开交由循环重连。
  void _resetEventsIdleWatchdog(int generation) {
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

  void _send(String data) {
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
      ScaffoldMessenger.of(context).showSnackBar(
        const SnackBar(content: Text('剪贴板没有文本')),
      );
      return;
    }
    _send(text);
  }

  Future<void> _createSession() async {
    try {
      final session =
          await _sessions.create(widget.project.id, worktreeId: widget.worktreeId);
      await _activateSession(session);
    } catch (_) {
      if (mounted && !_disposed) {
        ScaffoldMessenger.of(context).showSnackBar(
          const SnackBar(content: Text('新建会话失败，请稍后重试')),
        );
      }
    }
  }

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
    if (mounted) {
      ScaffoldMessenger.of(context).showSnackBar(const SnackBar(content: Text('已复制')));
    }
  }

  Future<void> _pickFavorite() async {
    final prompts =
        await PromptsClient(widget.http, widget.book.active!.baseUrl).listFavorites();
    if (!mounted) {
      return;
    }
    final chosen = await showModalBottomSheet<FavoritePrompt>(
      context: context,
      builder: (context) => ListView(
        children: [
          for (final prompt in prompts)
            ListTile(
              title: Text(prompt.title),
              subtitle: Text(prompt.content, maxLines: 2, overflow: TextOverflow.ellipsis),
              onTap: () => Navigator.pop(context, prompt),
            ),
        ],
      ),
    );
    if (chosen != null) {
      _send(chosen.content);
    }
  }

  Future<void> _optimize() async {
    final sessionId = _sessionId;
    if (sessionId == null) {
      return;
    }
    final controller = TextEditingController(text: _input.text);
    final prompt = await showDialog<String>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('Prompt 优化'),
        content: TextField(controller: controller, maxLines: 4),
        actions: [
          TextButton(onPressed: () => Navigator.pop(context), child: const Text('取消')),
          FilledButton(
            onPressed: () => Navigator.pop(context, controller.text.trim()),
            child: const Text('写入会话'),
          ),
        ],
      ),
    );
    if (prompt == null || prompt.isEmpty) {
      return;
    }
    await PromptsClient(widget.http, widget.book.active!.baseUrl).streamOptimizerToSession(
      prompt: prompt,
      sessionId: sessionId,
      workingDirectory: widget.project.path,
    );
  }

  Future<void> _pasteImage() async {
    final sessionId = _sessionId;
    if (sessionId == null) {
      return;
    }
    final picked = await ImagePicker().pickImage(source: ImageSource.gallery);
    if (picked == null) {
      return;
    }
    final bytes = await picked.readAsBytes();
    final b64 = base64Encode(bytes);
    final mime = picked.mimeType ?? 'image/jpeg';
    await _sessions.pasteImage(sessionId, 'data:$mime;base64,$b64');
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

  /// 会话 chip 条：横向滚动，每会话一个状态点 + 名称 chip，末尾「+ 新建」。
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
              child: ActionChip(
                avatar: CircleAvatar(
                  radius: 4,
                  backgroundColor: _sessionStatusColor(session.status),
                ),
                label: Text(session.displayName),
                onPressed: () => unawaited(_activateSession(session)),
              ),
            ),
          Padding(
            padding: const EdgeInsets.only(right: 8),
            child: ActionChip(
              avatar: const Icon(Icons.add, size: 16),
              label: const Text('新建'),
              onPressed: () => unawaited(_createSession()),
            ),
          ),
        ],
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    if (_error != null) {
      return Center(child: Text(_error!));
    }
    return Column(
      children: [
        Padding(
          padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 4),
          child: Row(
            children: [
              Expanded(child: Text(_status, style: Theme.of(context).textTheme.bodySmall)),
              IconButton(
                tooltip: '粘贴文本',
                onPressed: _pasteText,
                icon: const Icon(Icons.content_paste),
              ),
              IconButton(
                tooltip: '收藏 Prompt',
                onPressed: _pickFavorite,
                icon: const Icon(Icons.star_outline),
              ),
              IconButton(
                tooltip: 'Prompt 优化',
                onPressed: _optimize,
                icon: const Icon(Icons.auto_fix_high),
              ),
              IconButton(
                tooltip: '相册贴图',
                onPressed: _pasteImage,
                icon: const Icon(Icons.photo_outlined),
              ),
            ],
          ),
        ),
        _buildSessionChipBar(),
        Expanded(
          child: GestureDetector(
            onLongPress: _copySelection,
            child: TerminalView(_terminal, controller: _view),
          ),
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
                  controller: _input,
                  decoration: const InputDecoration(hintText: '输入后回车发送'),
                  onSubmitted: (value) {
                    _send('$value\r');
                    _input.clear();
                  },
                ),
              ),
              IconButton(
                onPressed: () {
                  _send('${_input.text}\r');
                  _input.clear();
                },
                icon: const Icon(Icons.send),
              ),
            ],
          ),
        ),
      ],
    );
  }
}
