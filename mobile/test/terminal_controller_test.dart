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
}
