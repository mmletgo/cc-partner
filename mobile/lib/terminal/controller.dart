/// One outbound input frame waiting for PTY ACK.
class PendingInput {
  const PendingInput({required this.id, required this.bytes});
  final String id;
  final String bytes;
}

enum TerminalSync { live, gapReplayRequired, replaying }

/// events 流重连退避起始间隔（1s → 2s → 4s → …）。
const int kEventsReconnectBaseDelayMs = 1000;

/// events 流重连退避上限。
const int kEventsReconnectMaxDelayMs = 15000;

/// events 流超过该时长未收到任何帧（含 heartbeat）即视为半开连接，主动断开重连。
const int kEventsIdleTimeoutMs = 45000;

/// 输入 WS 重连退避起始间隔。
const int kInputReconnectBaseDelayMs = 1000;

/// 输入 WS 重连退避上限。
const int kInputReconnectMaxDelayMs = 10000;

/// 业务逻辑：events 流与输入 WS 断开后都需要指数退避重连，退避节奏必须是可测试的纯函数。
///
/// Code Logic：attempt 0 返回 base，之后逐次翻倍，达到 max 后封顶；非法 attempt 直接返回 max。
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

/// Terminal channel policy for one session on the current PC.
class TerminalController {
  TerminalController({required this.sessionId});

  final String sessionId;
  final List<PendingInput> unacked = [];
  TerminalSync sync = TerminalSync.live;
  int nextInputId = 1;
  bool usedSessionsWriteFallback = false;

  static const inputSubprotocol = 'cc-partner.terminal-input.v1';
  static const inputPath = '/api/mobile/workbench/terminal-input-stream';
  static const eventsPath = '/api/workbench/events';
  static const pasteImagePath = '/api/mobile/workbench/sessions/paste-image';
  static const replayPath = '/api/mobile/workbench/sessions/replay';
  static const sessionsWritePath = '/api/mobile/workbench/sessions/write';

  /// Queue bytes on the input WS. ACK does not gate the next frame.
  String sendInput(String bytes) {
    final id = 'in-${nextInputId++}';
    unacked.add(PendingInput(id: id, bytes: bytes));
    return id;
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
  }

  String pasteImageRequestPath() => pasteImagePath;

  /// 业务逻辑：切换会话后旧会话未确认输入结果未知，且 gap/replay 状态不再适用于新会话。
  ///
  /// Code Logic：丢弃全部未确认输入（永不重放），并把同步状态强制恢复为 live。
  void resetForSessionSwitch() {
    takeUnackedOnDisconnect();
    finishReplay();
  }
}
