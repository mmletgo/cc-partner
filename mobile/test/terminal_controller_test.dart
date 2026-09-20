import 'dart:convert';

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
    terminal.onInputSocketOpened();
    terminal.handleInputFrame({'type': 'ready'});
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

  test('events 重连为固定 2s（任意 attempt 恒定），看门狗对齐 web 35s', () {
    expect(kEventsReconnectDelayMs, 2000);
    expect(kEventsWatchdogMs, 35000);
    // 兼容旧 UI 调用点：base/max 别名同值 ⇒ reconnectDelayMs 恒返回固定 2s。
    expect(kEventsReconnectBaseDelayMs, kEventsReconnectDelayMs);
    expect(kEventsReconnectMaxDelayMs, kEventsReconnectDelayMs);
    expect(kEventsIdleTimeoutMs, kEventsWatchdogMs);
    final delays = [
      for (var attempt = 0; attempt <= 6; attempt++)
        reconnectDelayMs(
          attempt,
          baseMs: kEventsReconnectBaseDelayMs,
          maxMs: kEventsReconnectMaxDelayMs,
        ),
    ];
    expect(delays, [2000, 2000, 2000, 2000, 2000, 2000, 2000]);
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
    terminal.onInputSocketOpened();
    terminal.handleInputFrame({'type': 'ready'});
    terminal.sendInput('a');
    expect(terminal.sync, TerminalSync.gapReplayRequired);
    expect(terminal.unacked, isNotEmpty);
    terminal.resetForSessionSwitch();
    expect(terminal.sync, TerminalSync.live);
    expect(terminal.unacked, isEmpty);
    expect(terminal.replayUnackedInput(), isEmpty);
  });

  group('输入 WS ready 握手', () {
    test('open ≠ 就绪：ready 帧之前拒绝发送，握手完成后放行', () {
      final terminal = TerminalController(sessionId: 's1');
      expect(terminal.inputStreamReady, isFalse);
      expect(terminal.sendInput('ls\n'), isNull);
      expect(terminal.unacked, isEmpty);

      // socket open 仍只是 connecting（等待 ready 握手）。
      terminal.onInputSocketOpened();
      expect(terminal.inputLink.state, TerminalInputLinkState.connecting);
      expect(terminal.inputStreamReady, isFalse);
      expect(terminal.sendInput('ls\n'), isNull);
      expect(terminal.unacked, isEmpty);

      expect(terminal.handleInputFrame({'type': 'ready'}), isTrue);
      expect(terminal.inputStreamReady, isTrue);
      expect(terminal.sendInput('ls\n'), 'in-1');
      expect(terminal.unacked, hasLength(1));
    });

    test('handleInputFrameText 解析 JSON；非法 JSON 封锁链路', () {
      final terminal = TerminalController(sessionId: 's1');
      expect(terminal.handleInputFrameText('{"type":"ready"}'), isTrue);
      expect(terminal.inputStreamReady, isTrue);

      expect(terminal.handleInputFrameText('not-json'), isTrue);
      expect(terminal.inputLinkBlocked, isTrue);
      expect(terminal.inputLink.message, '终端输入响应格式无效');
      expect(terminal.sendInput('a'), isNull);
    });

    test('未知帧类型返回 false（前向兼容，不改状态）', () {
      final terminal = TerminalController(sessionId: 's1');
      expect(terminal.handleInputFrame({'type': 'future-thing'}), isFalse);
      expect(terminal.inputLink.state, TerminalInputLinkState.connecting);
      expect(terminal.handleInputFrame('not-a-map'), isFalse);
    });

    test('状态变化经 onInputLinkChanged 回调导出', () {
      final terminal = TerminalController(sessionId: 's1');
      final states = <TerminalInputLinkState>[];
      terminal.onInputLinkChanged = (status) => states.add(status.state);
      terminal.onInputSocketOpened();
      terminal.handleInputFrame({'type': 'ready'});
      terminal.onInputSocketClosed();
      expect(states, [
        TerminalInputLinkState.connecting,
        TerminalInputLinkState.ready,
        TerminalInputLinkState.closed,
      ]);
    });
  });

  group('输入 WS 封锁与背压', () {
    TerminalController readyTerminal() {
      final terminal = TerminalController(sessionId: 's1');
      terminal.onInputSocketOpened();
      terminal.handleInputFrame({'type': 'ready'});
      return terminal;
    }

    test('单帧超过 32KiB：拒绝发送并封锁 lane，后续帧不允许继续发', () {
      final terminal = readyTerminal();
      final oversized = 'a' * (kMaxInputFrameBytes + 1);
      expect(terminal.sendInput(oversized), isNull);
      expect(terminal.unacked, isEmpty);
      expect(terminal.inputLinkBlocked, isTrue);
      expect(terminal.inputLink.message, contains('32 KiB'));
      // 封锁后普通帧也拒绝（不允许继续发）。
      expect(terminal.sendInput('ok'), isNull);
      expect(terminal.unacked, isEmpty);
    });

    test('单帧恰好 32KiB 不封锁', () {
      final terminal = readyTerminal();
      expect(terminal.sendInput('a' * kMaxInputFrameBytes), isNotNull);
      expect(terminal.inputLinkBlocked, isFalse);
    });

    test('在途字节超过 1MiB：背压封锁并给出封锁原因', () {
      final terminal = readyTerminal();
      final frame = 'x' * kMaxInputFrameBytes;
      // 32KiB × 32 = 1MiB 恰好可达；第 33 帧超限。
      for (var i = 0; i < 32; i++) {
        expect(terminal.sendInput(frame), isNotNull);
      }
      expect(terminal.unackedBytes, kMaxInputOutstandingBytes);
      expect(terminal.sendInput(frame), isNull);
      expect(terminal.inputLinkBlocked, isTrue);
      expect(terminal.inputLink.message, contains('背压'));
    });

    test('服务端 error 帧：清空在途并封锁，消息透传', () {
      final terminal = readyTerminal();
      terminal.sendInput('a');
      expect(terminal.handleInputFrame({'type': 'error', 'message': '鉴权失败'}), isTrue);
      expect(terminal.inputLinkBlocked, isTrue);
      expect(terminal.inputLink.message, '鉴权失败');
      expect(terminal.unacked, isEmpty);
      expect(terminal.sendInput('b'), isNull);
    });

    test('服务端 error 帧缺 message 时给默认文案', () {
      final terminal = readyTerminal();
      terminal.handleInputFrame({'type': 'error'});
      expect(terminal.inputLinkBlocked, isTrue);
      expect(terminal.inputLink.message, '终端输入被后端拒绝');
    });

    test('sendInputStrict：拒绝时抛 TerminalInputRefusedException，成功返回 id', () {
      final terminal = TerminalController(sessionId: 's1');
      expect(
        () => terminal.sendInputStrict('a'),
        throwsA(isA<TerminalInputRefusedException>()),
      );
      terminal.handleInputFrame({'type': 'ready'});
      expect(terminal.sendInputStrict('a'), 'in-1');
    });

    test('blockInputLink：UI 层 socket 错误可主动封锁并导出原因', () {
      final terminal = readyTerminal();
      terminal.blockInputLink('终端输入连接失败');
      expect(terminal.inputLinkBlocked, isTrue);
      expect(terminal.inputLink.message, '终端输入连接失败');
      expect(terminal.sendInput('a'), isNull);
    });
  });

  group('断线提示区分与 ack 核销', () {
    test('断线时有未确认输入：droppedUnackedOnDisconnect=true', () {
      final terminal = TerminalController(sessionId: 's1');
      terminal.onInputSocketOpened();
      terminal.handleInputFrame({'type': 'ready'});
      terminal.sendInput('a');
      terminal.onInputSocketClosed();
      expect(terminal.inputLink.state, TerminalInputLinkState.closed);
      expect(terminal.inputLink.droppedUnackedOnDisconnect, isTrue);
      expect(terminal.unacked, isEmpty);
    });

    test('断线时无未确认输入：droppedUnackedOnDisconnect=false', () {
      final terminal = TerminalController(sessionId: 's1');
      terminal.onInputSocketOpened();
      terminal.handleInputFrame({'type': 'ready'});
      terminal.onInputSocketClosed();
      expect(terminal.inputLink.state, TerminalInputLinkState.closed);
      expect(terminal.inputLink.droppedUnackedOnDisconnect, isFalse);
    });

    test('重连后恢复 connecting → ready，封锁解除', () {
      final terminal = TerminalController(sessionId: 's1');
      terminal.onInputSocketOpened();
      terminal.handleInputFrame({'type': 'ready'});
      terminal.blockInputLink('boom');
      expect(terminal.inputLinkBlocked, isTrue);
      terminal.onInputSocketOpened();
      expect(terminal.inputLinkBlocked, isFalse);
      expect(terminal.inputStreamReady, isFalse);
      terminal.handleInputFrame({'type': 'ready'});
      expect(terminal.inputStreamReady, isTrue);
      expect(terminal.sendInput('a'), isNotNull);
    });

    test('ack 帧按 seq 字符串核销在途输入（UI 可传 id 对齐服务端 seq）', () {
      final terminal = TerminalController(sessionId: 's1');
      terminal.onInputSocketOpened();
      terminal.handleInputFrame({'type': 'ready'});
      terminal.sendInput('a', id: '1');
      terminal.sendInput('b', id: '2');
      expect(terminal.unacked.map((e) => e.id), ['1', '2']);
      expect(terminal.handleInputFrame({'type': 'ack', 'seq': 1}), isTrue);
      expect(terminal.unacked.map((e) => e.id), ['2']);
      terminal.handleInputFrame({'type': 'ack', 'seq': 2});
      expect(terminal.unacked, isEmpty);
      expect(terminal.unackedBytes, 0);
    });

    test('背压按 UTF-8 字节计量（多字节字符不低估）', () {
      final terminal = TerminalController(sessionId: 's1');
      terminal.onInputSocketOpened();
      terminal.handleInputFrame({'type': 'ready'});
      // '中' 为 3 字节；byteLength 与 utf8 编码一致。
      final id = terminal.sendInput('中中中');
      expect(id, isNotNull);
      expect(terminal.unackedBytes, utf8.encode('中中中').length);
    });
  });

  group('replay ready 与 owner/hydration 支撑', () {
    test('finishReplay 置 replayReady，gap 重放后仍为 true，切会话清零', () {
      final terminal = TerminalController(sessionId: 's1');
      expect(terminal.replayReady, isFalse);
      terminal.finishReplay();
      expect(terminal.replayReady, isTrue);

      // gap → 重放完成：replayReady 保持 true（快照已重新取得）。
      terminal.onNdjsonLine({'type': 'gap'});
      terminal.beginReplay();
      terminal.finishReplay();
      expect(terminal.replayReady, isTrue);

      terminal.resetForSessionSwitch();
      expect(terminal.replayReady, isFalse);
    });

    test('ownerInstanceId 变化检出：/resume 换 owner 后允许重灌历史', () {
      final terminal = TerminalController(sessionId: 's1');
      expect(terminal.noteOwnerInstanceId('owner-1'), isTrue);
      expect(terminal.noteOwnerInstanceId('owner-1'), isFalse);
      expect(terminal.noteOwnerInstanceId('owner-2'), isTrue);
      expect(terminal.ownerInstanceId, 'owner-2');
    });

    test('markHistoryHydrated/isHistoryHydrated：owner 变化即视为未灌', () {
      final terminal = TerminalController(sessionId: 's1');
      expect(terminal.isHistoryHydrated(ownerInstanceId: 'o1'), isFalse);
      terminal.markHistoryHydrated(ownerInstanceId: 'o1');
      expect(terminal.isHistoryHydrated(ownerInstanceId: 'o1'), isTrue);
      // /resume 换 owner：重置语义 —— 视为未灌，允许重灌。
      expect(terminal.isHistoryHydrated(ownerInstanceId: 'o2'), isFalse);
      expect(terminal.isHistoryHydrated(ownerInstanceId: null), isFalse);
      terminal.markHistoryHydrated();
      expect(terminal.isHistoryHydrated(ownerInstanceId: null), isTrue);
      // 切会话后必须重新 hydration。
      terminal.resetForSessionSwitch();
      expect(terminal.isHistoryHydrated(ownerInstanceId: null), isFalse);
    });
  });

  test('启动会话选择：优先指定会话，再按 worktree/running 优先级矩阵', () {
    final exitedW1 = SessionSummary(
        id: 's0', projectId: 'p', name: 'old-w1', status: 'exited', worktreeId: 'w1');
    final runningW2 = SessionSummary(
        id: 's1', projectId: 'p', name: 'run-w2', status: 'running', worktreeId: 'w2');
    final exitedGlobal = SessionSummary(
        id: 's2', projectId: 'p', name: 'old', status: 'exited', worktreeId: 'w3');

    // preferredId 仍然最优先（既有恢复语义）。
    expect(
      pickPreferredSession([exitedW1, runningW2], preferredId: 's1')!.id,
      's1',
    );
    // 同 worktree running 最优先。
    expect(
      pickPreferredSession([exitedW1, runningW2, exitedGlobal], worktreeId: 'w2')!.id,
      's1',
    );
    // 同 worktree 仅 exited → 取同 worktree 任意。
    expect(
      pickPreferredSession([exitedW1, runningW2, exitedGlobal], worktreeId: 'w1')!.id,
      's0',
    );
    // 无 worktree 匹配 → 全局任意 running。
    expect(
      pickPreferredSession([exitedW1, runningW2, exitedGlobal], worktreeId: 'w9')!.id,
      's1',
    );
    // worktreeId 缺省 = 全局口径：任意 running。
    expect(pickPreferredSession([exitedW1, runningW2])!.id, 's1');
    // 全局无 running → 第一个（与 web sessions[0] 对齐，可为 exited）。
    expect(pickPreferredSession([exitedW1, exitedGlobal])!.id, 's0');
    // 空列表 → null，由调用方决定新建。
    expect(pickPreferredSession(const [], preferredId: 'missing'), isNull);
    expect(pickPreferredSession(const [], worktreeId: 'w1'), isNull);
  });
}
