import 'package:cc_partner_mobile/sessions/client.dart';
import 'package:cc_partner_mobile/terminal/controller.dart';
import 'package:test/test.dart';

void main() {
  test('gap frame forces replay before live continues', () {
    final terminal = TerminalController(sessionId: 's1');
    terminal.onNdjsonLine({'type': 'gap', 'afterSequence': 9});
    expect(terminal.sync, TerminalSync.gapReplayRequired);
    terminal.beginReplay();
    expect(terminal.sync, TerminalSync.replaying);
    terminal.finishReplay();
    expect(terminal.sync, TerminalSync.live);
  });

  test('unacked input is never auto-replayed after disconnect', () {
    final terminal = TerminalController(sessionId: 's1');
    terminal.sendInput('ls\n');
    terminal.sendInput('a');
    expect(terminal.unacked, hasLength(2));
    final leftover = terminal.takeUnackedOnDisconnect();
    expect(leftover, hasLength(2));
    expect(terminal.replayUnackedInput(), isEmpty);
    expect(terminal.unacked, isEmpty);
  });

  test('never falls back to /sessions/write', () {
    final terminal = TerminalController(sessionId: 's1');
    expect(terminal.mayUseSessionsWrite, isFalse);
    expect(TerminalController.sessionsWritePath, contains('sessions/write'));
    expect(TerminalController.inputSubprotocol, 'cc-partner.terminal-input.v1');
    expect(terminal.pasteImageRequestPath(), contains('paste-image'));
  });

  test('events 重连退避序列为 1s→2s→4s→8s→15s 封顶', () {
    final delays = [
      for (var attempt = 0; attempt <= 6; attempt++)
        reconnectDelayMs(
          attempt,
          baseMs: kEventsReconnectBaseDelayMs,
          maxMs: kEventsReconnectMaxDelayMs,
        ),
    ];
    expect(delays, [1000, 2000, 4000, 8000, 15000, 15000, 15000]);
    expect(kEventsIdleTimeoutMs, 45000);
  });

  test('输入 WS 重连退避序列为 1s→2s→4s→8s→10s 封顶', () {
    final delays = [
      for (var attempt = 0; attempt <= 5; attempt++)
        reconnectDelayMs(
          attempt,
          baseMs: kInputReconnectBaseDelayMs,
          maxMs: kInputReconnectMaxDelayMs,
        ),
    ];
    expect(delays, [1000, 2000, 4000, 8000, 10000, 10000]);
  });

  test('非法 attempt 直接取上限，避免翻倍下溢', () {
    expect(reconnectDelayMs(-1, baseMs: 1000, maxMs: 15000), 15000);
    expect(reconnectDelayMs(-7, baseMs: 1000, maxMs: 10000), 10000);
  });

  test('切换会话策略：不同会话/首次进入需要清屏重放，重复点击同一会话不需要', () {
    expect(shouldResetSessionView(currentSessionId: null, nextSessionId: 's2'), isTrue);
    expect(shouldResetSessionView(currentSessionId: '', nextSessionId: 's2'), isTrue);
    expect(shouldResetSessionView(currentSessionId: 's1', nextSessionId: 's2'), isTrue);
    expect(shouldResetSessionView(currentSessionId: 's1', nextSessionId: 's1'), isFalse);
    expect(shouldResetSessionView(currentSessionId: 's1', nextSessionId: ''), isFalse);
  });

  test('切换会话重置：未确认输入丢弃且同步状态回到 live', () {
    final terminal = TerminalController(sessionId: 's1');
    terminal.onNdjsonLine({'type': 'gap'});
    terminal.sendInput('a');
    expect(terminal.sync, TerminalSync.gapReplayRequired);
    expect(terminal.unacked, isNotEmpty);
    terminal.resetForSessionSwitch();
    expect(terminal.sync, TerminalSync.live);
    expect(terminal.unacked, isEmpty);
    expect(terminal.replayUnackedInput(), isEmpty);
  });

  test('启动会话选择：优先指定会话，其次首个非 exited，否则新建', () {
    final exited = SessionSummary(id: 's0', projectId: 'p', name: 'old', status: 'exited');
    final running = SessionSummary(id: 's1', projectId: 'p', name: 'run', status: 'running');
    expect(pickPreferredSession([exited, running], preferredId: 's1')!.id, 's1');
    expect(pickPreferredSession([exited, running])!.id, 's1');
    expect(pickPreferredSession([exited]), isNull);
    expect(pickPreferredSession(const [], preferredId: 'missing'), isNull);
  });
}
