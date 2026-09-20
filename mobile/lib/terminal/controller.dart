import 'dart:convert';

/// One outbound input frame waiting for PTY ACK.
class PendingInput {
  PendingInput({required this.id, required this.bytes, int? byteLength})
      : byteLength = byteLength ?? utf8.encode(bytes).length;

  final String id;
  final String bytes;

  /// 该帧的 UTF-8 字节数（背压按字节计量，对齐 web MAX_FRAME_BYTES 口径；缺省自动计算）。
  final int byteLength;
}

enum TerminalSync { live, gapReplayRequired, replaying }

/// events 流断线后的固定重连延迟（对齐 web WORKBENCH_HTTP_EVENT_RECONNECT_DELAY_MS = 2s）。
const int kEventsReconnectDelayMs = 2000;

/// events 流无帧看门狗超时（对齐 web WORKBENCH_HTTP_EVENT_WATCHDOG_MS = 35s）；
/// 超过该时长未收到任何帧（含 heartbeat）即视为半开连接，主动断开当前连接并走重连。
const int kEventsWatchdogMs = 35000;

/// 兼容旧 UI 调用点（terminal_page 仍以 base/max 形式调用 reconnectDelayMs）：
/// base 与 max 同值时任意 attempt 恒返回 2s，即 web 的固定重连节奏。
const int kEventsReconnectBaseDelayMs = kEventsReconnectDelayMs;

/// 兼容旧 UI 调用点：与 base 同值，保证固定 2s（不再指数退避）。
const int kEventsReconnectMaxDelayMs = kEventsReconnectDelayMs;

/// 兼容旧 UI 调用点：45s 空闲看门狗已对齐 web 收紧为 35s。
const int kEventsIdleTimeoutMs = kEventsWatchdogMs;

/// 输入 WS 重连退避起始间隔。
const int kInputReconnectBaseDelayMs = 1000;

/// 输入 WS 重连退避上限。
const int kInputReconnectMaxDelayMs = 10000;

/// 输入 WS 单帧字节上限 32KiB；超限拒绝发送并封锁通道（对齐 web MAX_FRAME_BYTES）。
const int kMaxInputFrameBytes = 32 * 1024;

/// 输入通道在途（已发送未 ACK）字节上限 1MiB；超限封锁通道（对齐 web MAX_LANE_OUTSTANDING_BYTES）。
const int kMaxInputOutstandingBytes = 1024 * 1024;

/// 历史 hydration（refreshHistory replay）请求超时；超时抛 TimeoutException 可重试
/// （对齐 web SCROLLBACK_HYDRATION_TIMEOUT_MS = 10s）。
const int kHistoryHydrationTimeoutMs = 10000;

/// 业务逻辑：events 流与输入 WS 断开后都需要自动重连，退避节奏必须是可测试的纯函数。
///
/// Code Logic：attempt 0 返回 base，之后逐次翻倍，达到 max 后封顶；非法 attempt 直接返回 max。
/// events 侧旧调用点以 base=max=2s 传入时恒返回 2s（web 固定节奏）。
int reconnectDelayMs(int attempt, {required int baseMs, required int maxMs}) {
  if (attempt < 0) {
    return maxMs;
  }
  var delay = baseMs;
  for (var i = 0; i < attempt; i++) {
    delay *= 2;
    if (delay >= maxMs) {
      return maxMs;
    }
  }
  return delay;
}

/// 业务逻辑：切换会话时旧画面属于上一个会话，必须清屏并重放快照，否则新会话输出串台。
///
/// Code Logic：当前无会话、下一会话 id 为空或两者不同时返回 true；重复点击同一会话返回 false。
bool shouldResetSessionView({
  required String? currentSessionId,
  required String nextSessionId,
}) {
  if (currentSessionId == null || currentSessionId.isEmpty) {
    return true;
  }
  if (nextSessionId.isEmpty) {
    return false;
  }
  return currentSessionId != nextSessionId;
}

/// 输入链路状态（对齐 web MobileTerminalInputStreamState）。
enum TerminalInputLinkState {
  /// 连接建立中：含 WS 已 open 但尚未收到服务端 `ready` 握手（open ≠ 就绪）。
  connecting,

  /// 已收到服务端 `ready` 握手，可以发送输入。
  ready,

  /// 已封锁（帧超限 / 背压超限 / 服务端 error 帧 / 链路错误）；
  /// 仅重建连接并重新握手后恢复，message 携带封锁原因。
  blocked,

  /// 连接已断开；是否有未确认输入被丢弃见 [TerminalInputLinkStatus.droppedUnackedOnDisconnect]。
  closed,
}

/// 输入链路状态快照（UI 渲染素材；具体文案由 UI 层拼接）。
class TerminalInputLinkStatus {
  const TerminalInputLinkStatus(
    this.state, {
    this.message,
    this.droppedUnackedOnDisconnect = false,
  });

  final TerminalInputLinkState state;

  /// 封锁 / 错误原因（blocked 时非空；closed 时为 null，由 UI 按
  /// [droppedUnackedOnDisconnect] 拼「未确认输入已丢弃」或「无未确认输入」文案）。
  final String? message;

  /// 断线区分素材：true = 断线时有未确认输入被丢弃（永不自动重放）；false = 无未确认输入。
  final bool droppedUnackedOnDisconnect;
}

/// [TerminalController.sendInputStrict] 在链路拒绝发送时抛出（对齐 web enqueue 抛错语义）。
class TerminalInputRefusedException implements Exception {
  const TerminalInputRefusedException(this.message);

  /// 拒绝原因（与 inputLink.message 同源）。
  final String message;

  @override
  String toString() => message;
}

/// Terminal channel policy for one session on the current PC.
class TerminalController {
  TerminalController({required this.sessionId});

  final String sessionId;
  final List<PendingInput> unacked = [];
  TerminalSync sync = TerminalSync.live;
  int nextInputId = 1;
  bool usedSessionsWriteFallback = false;

  /// 输入链路当前状态快照（UI 经 [onInputLinkChanged] 或轮询读取）。
  TerminalInputLinkStatus inputLink =
      const TerminalInputLinkStatus(TerminalInputLinkState.connecting);

  /// 输入链路状态变化回调（UI 借此刷新输入禁用态 / 断线提示；可空）。
  void Function(TerminalInputLinkStatus status)? onInputLinkChanged;

  /// 当前会话是否已收到过权威 replay 快照（对齐 web replayReady 门控：
  /// hydration 不得在 initial replay 完成前发起）。
  bool replayReady = false;

  /// 最近一次快照 / 事件流观察到的 ownerInstanceId；变化说明桌面端 /resume 换了 owner。
  String? ownerInstanceId;

  String? _historyHydratedSessionId;
  String? _historyHydratedOwnerInstanceId;

  static const inputSubprotocol = 'cc-partner.terminal-input.v1';
  static const inputPath = '/api/mobile/workbench/terminal-input-stream';
  static const eventsPath = '/api/workbench/events';
  static const pasteImagePath = '/api/mobile/workbench/sessions/paste-image';
  static const replayPath = '/api/mobile/workbench/sessions/replay';
  static const sessionsWritePath = '/api/mobile/workbench/sessions/write';

  /// 输入链路是否完成 ready 握手（UI 应在 false 时禁用输入）。
  bool get inputStreamReady => inputLink.state == TerminalInputLinkState.ready;

  /// 输入链路是否已封锁（超帧 / 背压 / error 帧 / 链路错误；需重连恢复）。
  bool get inputLinkBlocked => inputLink.state == TerminalInputLinkState.blocked;

  /// 在途（已发送未 ACK）输入的总字节数，用于背压判断（web outstandingBytes 同口径）。
  int get unackedBytes {
    var total = 0;
    for (final item in unacked) {
      total += item.byteLength;
    }
    return total;
  }

  /// Queue bytes on the input WS. ACK does not gate the next frame.
  ///
  /// 门控（对齐 web enqueue）：链路未 ready 握手时拒绝；单帧超过 32KiB 拒绝并封锁通道；
  /// 在途字节超过 1MiB 拒绝并封锁通道。拒绝时返回 null（不排队、不抛错），封锁原因读
  /// [inputLink]。旧调用点忽略返回值仍兼容；严格抛错语义用 [sendInputStrict]。
  /// [id] 可选：UI 传服务端 seq 的字符串形式作为待确认 id，ack 帧即可精确核销；
  /// 缺省沿用 `in-<n>` 本地编号。
  String? sendInput(String bytes, {String? id}) {
    if (!inputStreamReady) {
      return null;
    }
    final frameBytes = utf8.encode(bytes).length;
    if (frameBytes > kMaxInputFrameBytes) {
      _publishInputLink(const TerminalInputLinkStatus(
        TerminalInputLinkState.blocked,
        message: '单个终端输入帧超过 32 KiB',
      ));
      return null;
    }
    if (unackedBytes + frameBytes > kMaxInputOutstandingBytes) {
      _publishInputLink(const TerminalInputLinkStatus(
        TerminalInputLinkState.blocked,
        message: '终端输入背压超过安全上限，通道已封锁',
      ));
      return null;
    }
    final pendingId = id ?? 'in-${nextInputId++}';
    unacked.add(PendingInput(id: pendingId, bytes: bytes, byteLength: frameBytes));
    return pendingId;
  }

  /// sendInput 的严格变体：拒绝时抛 [TerminalInputRefusedException]（web 同语义），成功返回 id。
  String sendInputStrict(String bytes, {String? id}) {
    final pendingId = sendInput(bytes, id: id);
    if (pendingId == null) {
      throw TerminalInputRefusedException(
          inputLink.message ?? '终端输入流尚未就绪');
    }
    return pendingId;
  }

  void onAck(String id) {
    unacked.removeWhere((item) => item.id == id);
  }

  /// Disconnect: unacked input is unknown. Never auto-replay. Never use /sessions/write.
  List<PendingInput> takeUnackedOnDisconnect() {
    final leftover = List<PendingInput>.from(unacked);
    unacked.clear();
    return leftover;
  }

  List<String> replayUnackedInput() {
    // Spec: forbidden.
    return const [];
  }

  bool get mayUseSessionsWrite => false;

  /// 输入 WS 已建立（socket open）：仅进入「等待 ready 握手」，不得视为可发送。
  void onInputSocketOpened() {
    _publishInputLink(
        const TerminalInputLinkStatus(TerminalInputLinkState.connecting));
  }

  /// UI 层 socket error / hello 发送失败时主动封锁通道（文案由调用方给）。
  void blockInputLink(String message) {
    _publishInputLink(TerminalInputLinkStatus(
      TerminalInputLinkState.blocked,
      message: message,
    ));
  }

  /// 输入 WS 断开：丢弃未确认输入（永不重放），并把「断线时是否有未确认输入被丢弃」
  /// 作为文案素材导出（web uncertain 区分两种提示，文案由 UI 层拼）。
  void onInputSocketClosed() {
    final dropped = unacked.isNotEmpty;
    takeUnackedOnDisconnect();
    _publishInputLink(TerminalInputLinkStatus(
      TerminalInputLinkState.closed,
      droppedUnackedOnDisconnect: dropped,
    ));
  }

  /// 解析输入 WS 的 JSON 文本帧；非法 JSON 视为协议错误并封锁（对齐 web
  /// '终端输入响应格式无效'）。返回是否按输入帧处理。
  bool handleInputFrameText(String raw) {
    Object? decoded;
    try {
      decoded = jsonDecode(raw);
    } catch (_) {
      blockInputLink('终端输入响应格式无效');
      return true;
    }
    return handleInputFrame(decoded);
  }

  /// 处理输入 WS 已解码的一帧：`ready` → 握手完成；`ack` → 按 seq 核销在途输入；
  /// `error` → 清空在途并封锁通道（消息透传给 UI）。未知类型返回 false（前向兼容）。
  bool handleInputFrame(Object? frame) {
    if (frame is! Map) {
      return false;
    }
    final type = frame['type'];
    if (type == 'ready') {
      _publishInputLink(
          const TerminalInputLinkStatus(TerminalInputLinkState.ready));
      return true;
    }
    if (type == 'ack') {
      final seq = frame['seq'];
      if (seq != null) {
        onAck('$seq');
      }
      return true;
    }
    if (type == 'error') {
      takeUnackedOnDisconnect();
      final rawMessage = frame['message'];
      final message =
          rawMessage is String && rawMessage.isNotEmpty ? rawMessage : '终端输入被后端拒绝';
      _publishInputLink(TerminalInputLinkStatus(
        TerminalInputLinkState.blocked,
        message: message,
      ));
      return true;
    }
    return false;
  }

  void _publishInputLink(TerminalInputLinkStatus next) {
    inputLink = next;
    onInputLinkChanged?.call(next);
  }

  void onNdjsonLine(Map<String, dynamic> frame) {
    final type = frame['type'] as String? ?? frame['kind'] as String? ?? '';
    if (type == 'gap') {
      sync = TerminalSync.gapReplayRequired;
    }
  }

  /// After gap: list sessions → replay snapshot → then live. Ignore-and-continue is forbidden.
  void beginReplay() {
    if (sync != TerminalSync.gapReplayRequired) {
      return;
    }
    sync = TerminalSync.replaying;
  }

  void finishReplay() {
    sync = TerminalSync.live;
    replayReady = true;
  }

  String pasteImageRequestPath() => pasteImagePath;

  /// 记录事件流 / 快照观察到的 ownerInstanceId；与上次不同（桌面端 /resume 换 owner）
  /// 时返回 true，UI 借此重置「已灌历史」标记允许重灌。
  bool noteOwnerInstanceId(String? owner) {
    final changed = ownerInstanceId != owner;
    ownerInstanceId = owner;
    return changed;
  }

  /// 标记当前会话已用 owner 端 tmux 历史灌过本地 buffer（hydration 成功后调用）。
  void markHistoryHydrated({String? ownerInstanceId}) {
    _historyHydratedSessionId = sessionId;
    _historyHydratedOwnerInstanceId = ownerInstanceId;
  }

  /// 当前会话是否已灌历史；ownerInstanceId 与标记时不一致（/resume 换 owner）时返回
  /// false 允许重灌（对齐 web hydratedScrollbackSessionRef + activeAgentIdentity 重置）。
  bool isHistoryHydrated({String? ownerInstanceId}) {
    return _historyHydratedSessionId == sessionId &&
        _historyHydratedOwnerInstanceId == ownerInstanceId;
  }

  /// 业务逻辑：切换会话后旧会话未确认输入结果未知，且 gap/replay 状态不再适用于新会话。
  ///
  /// Code Logic：丢弃全部未确认输入（永不重放），把同步状态强制恢复为 live，
  /// 并清掉 replayReady 与「已灌历史」标记（新会话必须重新取得权威快照）。
  void resetForSessionSwitch() {
    takeUnackedOnDisconnect();
    finishReplay();
    replayReady = false;
    _historyHydratedSessionId = null;
    _historyHydratedOwnerInstanceId = null;
  }
}
