/// One outbound input frame waiting for PTY ACK.
class PendingInput {
  const PendingInput({required this.id, required this.bytes});
  final String id;
  final String bytes;
}

enum TerminalSync { live, gapReplayRequired, replaying }

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
}
