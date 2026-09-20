import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

import 'package:cc_partner_mobile/address_book/book.dart';
import 'package:cc_partner_mobile/address_book/models.dart';
import 'package:cc_partner_mobile/core/lan_http.dart';
import 'package:cc_partner_mobile/git/client.dart';
import 'package:cc_partner_mobile/prompts/client.dart';
import 'package:cc_partner_mobile/projects/client.dart';
import 'package:cc_partner_mobile/sessions/client.dart';
import 'package:cc_partner_mobile/terminal/controller.dart';
import 'package:cc_partner_mobile/ui/terminal_page.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

SessionSummary _s(
  String id, {
  String status = 'running',
  bool panes = false,
  int paneCount = 0,
}) =>
    SessionSummary(
      id: id,
      projectId: 'p1',
      name: id,
      status: status,
      supportsPanes: panes,
      paneCount: paneCount,
    );

AddressBook _book([String baseUrl = 'http://127.0.0.1:1']) {
  final book = AddressBook(store: MemoryAddressBookStore());
  book.servers.add(ServerRecord(
    id: 'pc',
    host: '127.0.0.1',
    port: 1,
    baseUrl: baseUrl,
  ));
  book.activeServerId = 'pc';
  return book;
}

class _FakeSessions extends SessionsClient {
  _FakeSessions(this.sessions) : super(LanHttpClient(), 'http://127.0.0.1:1');

  List<SessionSummary> sessions;
  final List<String> closedIds = [];
  final List<String> replayIds = [];
  final List<String> focusedIds = [];
  int zoomCalls = 0;
  int splitCalls = 0;
  int switchCalls = 0;
  int closePaneCalls = 0;
  bool closePaneClosesWindow = false;

  @override
  Future<List<SessionSummary>> list(String projectId) async => sessions;

  @override
  Future<void> close(String sessionId) async {
    closedIds.add(sessionId);
    // 模拟后端：关闭后权威列表同步移除该会话。
    sessions = List<SessionSummary>.from(sessions)
      ..removeWhere((item) => item.id == sessionId);
  }

  @override
  Future<void> focus(String sessionId) async {
    focusedIds.add(sessionId);
  }

  @override
  Future<Map<String, dynamic>> replay(String sessionId,
      {bool refreshHistory = false}) async {
    replayIds.add(sessionId);
    return {'sessionId': sessionId, 'buffer': '', 'lastSeq': 0};
  }

  @override
  Future<void> zoomPane(String sessionId) async {
    zoomCalls += 1;
  }

  @override
  Future<void> splitPane(String sessionId, {String direction = 'down'}) async {
    splitCalls += 1;
  }

  @override
  Future<void> switchPane(String sessionId) async {
    switchCalls += 1;
  }

  @override
  Future<ClosePaneResult> closePane(String sessionId) async {
    closePaneCalls += 1;
    return ClosePaneResult(sessionId: sessionId, closedWindow: closePaneClosesWindow);
  }
}

/// boot 失败可切换的 fake：listError 非 null 时 list 抛错，模拟 boot 失败。
class _FailingListSessions extends _FakeSessions {
  _FailingListSessions(super.sessions);

  Object? listError = Exception('boot 失败');

  @override
  Future<List<SessionSummary>> list(String projectId) async {
    final error = listError;
    if (error != null) {
      throw error;
    }
    return sessions;
  }
}

/// 初始 replay 挂起的 fake：用 Completer 控制 replay 完成时机，用于门闩测试。
class _GatedReplaySessions extends _FakeSessions {
  _GatedReplaySessions(super.sessions);

  final Completer<Map<String, dynamic>> initialReplay =
      Completer<Map<String, dynamic>>();

  @override
  Future<Map<String, dynamic>> replay(String sessionId,
      {bool refreshHistory = false}) {
    replayIds.add(sessionId);
    return initialReplay.future;
  }
}

/// close 后把剩余会话置为 exited 的 fake：借关窗流程末尾的 _refreshSessions
/// 让权威列表带回非 running 状态，驱动输入行 running 条件的禁用/恢复断言。
class _ExitedAfterCloseSessions extends _FakeSessions {
  _ExitedAfterCloseSessions(super.sessions);

  /// true 时 close 后剩余会话全部标为 exited（模拟权威列表刷新发现会话已退出）。
  bool markExited = false;

  @override
  Future<void> close(String sessionId) async {
    await super.close(sessionId);
    if (markExited) {
      sessions = [
        for (final s in sessions)
          SessionSummary(
            id: s.id,
            projectId: s.projectId,
            name: s.name,
            status: 'exited',
            worktreeId: s.worktreeId,
            supportsPanes: s.supportsPanes,
            paneCount: s.paneCount,
          ),
      ];
    }
  }
}

/// 测试内存 Socket：无真实 IO，`add` 计数用于断言「是否向外发送过字节」。
class _FakeSocket extends Stream<Uint8List> implements Socket {
  final StreamController<Uint8List> _incoming = StreamController<Uint8List>();
  final Completer<void> _done = Completer<void>();
  int writeCount = 0;

  /// 模拟对端关闭：结束入站流，驱动 WebSocket 的 onDone。
  void closePeer() {
    _incoming.close();
  }

  @override
  StreamSubscription<Uint8List> listen(
    void Function(Uint8List data)? onData, {
    Function? onError,
    void Function()? onDone,
    bool? cancelOnError,
  }) {
    return _incoming.stream.listen(
      onData,
      onError: onError,
      onDone: onDone,
      cancelOnError: cancelOnError ?? false,
    );
  }

  @override
  void add(List<int> bytes) {
    writeCount += 1;
  }

  @override
  Future<void> addStream(Stream<List<int>> stream) async {
    await for (final _ in stream) {
      writeCount += 1;
    }
  }

  @override
  Future<dynamic> close() async {
    // 同时结束入站流：让 _WebSocketImpl 的 close 握手完整走完，
    // 取消其 5s 强制关闭 Timer，避免测试结束时残留 pending Timer。
    await _incoming.close();
    if (!_done.isCompleted) {
      _done.complete();
    }
  }

  @override
  Future<Socket> get done => _done.future.then((_) => this);

  @override
  void addError(Object error, [StackTrace? stackTrace]) {}

  @override
  Encoding get encoding => utf8;

  @override
  set encoding(Encoding value) {}

  @override
  Future<void> flush() async {}

  @override
  void write(Object? object) {}

  @override
  void writeAll(Iterable<Object?> objects, [String separator = '']) {}

  @override
  void writeCharCode(int charCode) {}

  @override
  void writeln([Object? object = '']) {}

  @override
  void destroy() {
    if (!_done.isCompleted) {
      _done.complete();
    }
  }

  @override
  bool setOption(SocketOption option, bool enabled) => true;

  @override
  Uint8List getRawOption(RawSocketOption option) => Uint8List(0);

  @override
  void setRawOption(RawSocketOption option) {}

  @override
  InternetAddress get address => InternetAddress.loopbackIPv4;

  @override
  InternetAddress get remoteAddress => InternetAddress.loopbackIPv4;

  @override
  int get port => 0;

  @override
  int get remotePort => 0;
}

/// openWebSocket 直接交付预建 Socket（包装为 WebSocket）或抛错的 http stub，绕开真实网络。
class _StubWebSocketHttp extends LanHttpClient {
  _StubWebSocketHttp(this.factory) : super();

  /// 返回 null 表示连接失败（模拟连接被拒），否则交付已升级的内存 Socket。
  final Socket? Function() factory;
  int connectCalls = 0;

  @override
  Future<WebSocket> openWebSocket(
    String baseUrl,
    String path, {
    Iterable<String>? protocols,
  }) {
    connectCalls += 1;
    final socket = factory();
    if (socket == null) {
      return Future<WebSocket>.error(Exception('input ws unavailable'));
    }
    return Future<WebSocket>.value(
      WebSocket.fromUpgradedSocket(
        socket,
        protocol: TerminalController.inputSubprotocol,
        serverSide: false,
      ),
    );
  }
}

class _FakeGit extends GitClient {
  _FakeGit() : super(LanHttpClient(), 'http://127.0.0.1:1');

  Map<String, dynamic> commitResult = {'kind': 'succeeded', 'value': <String, dynamic>{}};
  Map<String, dynamic> mergeResult = {'kind': 'succeeded', 'value': <String, dynamic>{}};
  Map<String, dynamic> repairResult = {'terminalSessionId': 's-repair'};

  /// mutation-operation（ledger 对账）返回脚本；null 表示后端无记录。
  Map<String, dynamic>? Function(String operationId)? ledgerScript;
  List<Map<String, dynamic>> trees = const [];
  int commitCalls = 0;
  int mergeCalls = 0;
  int repairCalls = 0;
  String? lastCommitMessage;
  final List<String> commitOperationIds = [];
  final List<String> mergeOperationIds = [];
  final List<String> ledgerQueryIds = [];

  @override
  Future<Map<String, dynamic>> commit({
    required String worktreeId,
    required String clientOperationId,
    String? message,
  }) async {
    commitCalls += 1;
    commitOperationIds.add(clientOperationId);
    lastCommitMessage = message;
    // unknown envelope 回显请求 id（后端语义），保证对账同 id。
    if (commitResult['kind'] == 'unknown') {
      return {'kind': 'unknown', 'clientOperationId': clientOperationId};
    }
    return commitResult;
  }

  @override
  Future<Map<String, dynamic>> merge({
    required String projectId,
    required String worktreeId,
    required String clientOperationId,
  }) async {
    mergeCalls += 1;
    mergeOperationIds.add(clientOperationId);
    if (mergeResult['kind'] == 'unknown') {
      return {'kind': 'unknown', 'clientOperationId': clientOperationId};
    }
    return mergeResult;
  }

  @override
  Future<Map<String, dynamic>> repairHookFailure({
    required String worktreeId,
    required Map<String, dynamic> hookFailure,
  }) async {
    repairCalls += 1;
    return repairResult;
  }

  @override
  Future<Map<String, dynamic>?> mutationOperation(String clientOperationId) async {
    ledgerQueryIds.add(clientOperationId);
    final ledger = ledgerScript?.call(clientOperationId);
    return ledger == null ? null : Map<String, dynamic>.from(ledger);
  }

  @override
  Future<Map<String, dynamic>> listWorktrees(
    String projectId, {
    bool includeGitStatus = false,
  }) async {
    return {'ok': true, 'worktrees': [for (final tree in trees) Map<String, dynamic>.from(tree)]};
  }
}

class _FakePrompts extends PromptsClient {
  _FakePrompts({this.favorites, this.error}) : super(LanHttpClient(), 'http://127.0.0.1:1');

  List<FavoritePrompt>? favorites;
  Object? error;
  final List<String> optimized = [];
  final List<String?> workingDirectories = [];

  @override
  Future<List<FavoritePrompt>> listFavorites() async {
    if (error != null) {
      throw error!;
    }
    return favorites ?? const [];
  }

  @override
  Future<Map<String, dynamic>> streamOptimizerToSession({
    required String prompt,
    required String sessionId,
    String? workingDirectory,
    String targetLanguage = 'zh',
  }) async {
    optimized.add(prompt);
    workingDirectories.add(workingDirectory);
    return <String, dynamic>{};
  }
}

ProjectSummary get _project => const ProjectSummary(id: 'p1', name: 'demo', path: '/tmp/demo');

/// 按 icon 定位工具行 IconButton（byTooltip 命中的是 Tooltip 本体，不能直接 cast）。
IconButton _iconButton(WidgetTester tester, IconData icon) {
  return tester.widget<IconButton>(
    find.ancestor(of: find.byIcon(icon), matching: find.byType(IconButton)).first,
  );
}

Future<void> _pump(
  WidgetTester tester, {
  required SessionsClient sessions,
  PromptsClient? prompts,
  GitClient? git,
  LanHttpClient? http,
  String? baseUrl,
  ValueChanged<bool>? onFullscreenChanged,
  VoidCallback? onWorktreesMutated,
  Map<String, dynamic>? worktreeInfo,
  String? worktreePath,
}) async {
  await tester.pumpWidget(MaterialApp(
    home: Scaffold(
      // key 随 worktreeInfo 变化：模拟 shell 的 ValueKey 换树行为，避免同类型 widget
      // 复用旧 State（旧 State 的 _git 注入实例会过期，导致断言打到旧 fake 上）。
      body: TerminalPage(
        key: ValueKey('terminal-under-test-$worktreeInfo'),
        book: _book(baseUrl ?? 'http://127.0.0.1:1'),
        http: http ?? LanHttpClient(),
        project: _project,
        worktreeId: 'w1',
        worktreeInfo: worktreeInfo,
        worktreePath: worktreePath,
        onFullscreenChanged: onFullscreenChanged,
        onWorktreesMutated: onWorktreesMutated,
        sessionsClient: sessions,
        promptsClient: prompts,
        gitClient: git,
        backgroundTimersDisabled: true,
      ),
    ),
  ));
  // boot（list + activate + replay）需要几帧；再泵过 80ms resize 防抖。
  await tester.pump();
  await tester.pump();
  await tester.pump(const Duration(milliseconds: 150));
}

void main() {
  testWidgets('会话 chip 关闭：关闭非当前会话仅移除 chip，不触发切换', (tester) async {
    final sessions = _FakeSessions([_s('s0'), _s('s1', status: 'exited')]);
    await _pump(tester, sessions: sessions);
    expect(find.widgetWithText(InputChip, 's0'), findsOneWidget);

    final chips = find.byType(InputChip);
    expect(chips, findsNWidgets(2));
    // xterm/chip 的删除图标在渲染层内部，用 delete tooltip 定位最稳妥。
    await tester.tap(find.byTooltip('关闭窗口').at(1));
    await tester.pump();
    await tester.pump();

    expect(sessions.closedIds, ['s1']);
    // 未切换会话：replay 只发生在初始 s0。
    expect(sessions.replayIds, ['s0']);
    await tester.pump();
    expect(find.byType(InputChip), findsOneWidget);
  });

  testWidgets('会话 chip 关闭：关闭当前会话后按优先级切到下一个并重放', (tester) async {
    final sessions = _FakeSessions([_s('s0'), _s('s1')]);
    await _pump(tester, sessions: sessions);

    await tester.tap(find.byTooltip('关闭窗口').first);
    await tester.pump();
    await tester.pump();
    await tester.pump();

    expect(sessions.closedIds, ['s0']);
    expect(sessions.replayIds.contains('s1'), isTrue);
  });

  testWidgets('全屏：隐藏 chip 条并回调壳层，退出恢复', (tester) async {
    final sessions = _FakeSessions([_s('s0')]);
    final fullscreenEvents = <bool>[];
    await _pump(tester, sessions: sessions, onFullscreenChanged: fullscreenEvents.add);

    await tester.tap(find.byTooltip('全屏'));
    await tester.pump();
    expect(fullscreenEvents, [true]);
    expect(find.byType(InputChip), findsNothing);
    expect(find.byTooltip('退出全屏'), findsOneWidget);

    await tester.tap(find.byTooltip('退出全屏'));
    await tester.pump();
    expect(fullscreenEvents, [true, false]);
    expect(find.byType(InputChip), findsOneWidget);
  });

  testWidgets('窗格菜单：split down + zoom + 刷新，switch 仅多 pane 可用', (tester) async {
    final sessions = _FakeSessions([_s('s0', panes: true, paneCount: 1)]);
    await _pump(tester, sessions: sessions);

    await tester.tap(find.byTooltip('窗格'));
    await tester.pumpAndSettle();
    expect(find.text('新增窗格'), findsOneWidget);
    expect(find.text('切换窗格'), findsOneWidget);

    await tester.tap(find.text('切换窗格'));
    await tester.pump();
    expect(sessions.switchCalls, 0);

    await tester.tap(find.text('新增窗格'));
    await tester.pump();
    await tester.pump();
    expect(sessions.splitCalls, 1);
    // split 后 zoom 幂等调用（s0 running + supportsPanes）。
    expect(sessions.zoomCalls, greaterThanOrEqualTo(1));
  });

  testWidgets('窗格菜单：session 不支持 panes 时全部动作不可用', (tester) async {
    final sessions = _FakeSessions([_s('s0')]);
    await _pump(tester, sessions: sessions);

    await tester.tap(find.byTooltip('窗格'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('新增窗格'));
    await tester.pump();
    expect(sessions.splitCalls, 0);
  });

  testWidgets('提交：空 message → null（AI 生成）；failedHook → 修复卡 → AI 修复切会话', (tester) async {
    final sessions = _FakeSessions([_s('s0'), _s('s-repair')]);
    final git = _FakeGit()
      ..commitResult = {
        'kind': 'failedHook',
        'clientOperationId': 'op-1',
        'hookFailure': {'stage': 'preCommit', 'stdout': 'lint failed', 'exitCode': 1},
      };
    await _pump(tester, sessions: sessions, git: git);

    await tester.tap(find.byTooltip('提交'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('提交').last);
    await tester.pumpAndSettle();

    expect(git.commitCalls, 1);
    expect(git.lastCommitMessage, isNull);
    expect(find.text('pre-commit 钩子阻止 commit'), findsOneWidget);
    expect(find.text('展开钩子输出'), findsOneWidget);

    await tester.tap(find.text('让 AI 修复'));
    await tester.pumpAndSettle();
    expect(git.repairCalls, 1);
    // 修复返回 terminalSessionId=s-repair 且在列表中 → 自动切换（replay）。
    expect(sessions.replayIds.contains('s-repair'), isTrue);
    expect(find.text('修复已在新的终端 tab 里运行，切换过去查看进度。'), findsOneWidget);
  });

  testWidgets('合并：确认对话框 → merge 成功 → SnackBar 与壳层回调', (tester) async {
    final sessions = _FakeSessions([_s('s0')]);
    final git = _FakeGit();
    var mutated = 0;
    await _pump(
      tester,
      sessions: sessions,
      git: git,
      onWorktreesMutated: () => mutated += 1,
      // shell 按当前 worktree 从权威列表取出 DTO 传入（B9 接缝契约）。
      worktreeInfo: const {
        'id': 'w1',
        'name': 'w1',
        'branch': 'feat/x',
        'isMain': false,
      },
    );

    await tester.tap(find.byTooltip('合并'));
    await tester.pumpAndSettle();
    expect(find.text('确定把「w1」合并到主工作区？'), findsOneWidget);

    await tester.tap(find.widgetWithText(FilledButton, '合并'));
    await tester.pumpAndSettle();
    expect(git.mergeCalls, 1);
    expect(mutated, 1);
    expect(find.text('合并成功'), findsOneWidget);
    // 让 SnackBar 的自动消失 Timer 走完，避免测试结束残留 pending Timer。
    await tester.pump(const Duration(milliseconds: 3200));
  });

  testWidgets('B9 合并门控：主工作区默认分支禁用；可收集/分支不同才开放', (tester) async {
    final sessions = _FakeSessions([_s('s0')]);

    // 无 worktreeInfo（shell 未取出）→ 禁用。
    await _pump(tester, sessions: sessions, git: _FakeGit());
    expect(_iconButton(tester, Icons.merge_type).onPressed, isNull);

    // 主工作区默认分支（无 collect、branch==homeBranch）→ 禁用并给出说明。
    await _pump(
      tester,
      sessions: sessions,
      git: _FakeGit(),
      worktreeInfo: const {
        'id': 'w-main',
        'name': 'main',
        'branch': 'main',
        'homeBranch': 'main',
        'isMain': true,
      },
    );
    expect(find.byTooltip('主工作区默认分支，无需合并'), findsOneWidget);
    expect(_iconButton(tester, Icons.merge_type).onPressed, isNull);

    // 主工作区可 collect-merge → 开放，确认文案用 collect 专用文案。
    final git = _FakeGit();
    await _pump(
      tester,
      sessions: sessions,
      git: git,
      worktreeInfo: const {
        'id': 'w-main',
        'name': 'main',
        'branch': 'main',
        'homeBranch': 'main',
        'isMain': true,
        'canCollectMerge': true,
        'collectibleBranches': ['feat/x'],
      },
    );
    await tester.tap(find.byTooltip('合并'));
    await tester.pumpAndSettle();
    expect(
      find.text('确定把本工作区的 1 条分支（feat/x）合并到「main」，并切回该主分支？'),
      findsOneWidget,
    );
    await tester.tap(find.widgetWithText(FilledButton, '合并'));
    await tester.pumpAndSettle();
    expect(git.mergeCalls, 1);
    await tester.pump(const Duration(milliseconds: 3200));
  });

  testWidgets('B2 commit unknown → 同 id 查 ledger 成功 → 提交成功并回写壳层', (tester) async {
    final sessions = _FakeSessions([_s('s0')]);
    final git = _FakeGit()
      ..commitResult = {'kind': 'unknown', 'clientOperationId': 'srv-op-1'}
      ..ledgerScript = (operationId) => {
            'state': 'succeeded',
            'intent': {'kind': 'commit'},
          };
    var mutated = 0;
    await _pump(tester, sessions: sessions, git: git, onWorktreesMutated: () => mutated += 1);

    await tester.tap(find.byTooltip('提交'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('提交').last);
    await tester.pumpAndSettle();

    // 只发了一次 commit；对账用同一 clientOperationId 查 ledger。
    expect(git.commitCalls, 1);
    expect(git.ledgerQueryIds, [git.commitOperationIds.first]);
    expect(mutated, 1);
    expect(find.text('提交成功'), findsOneWidget);
    expect(find.byKey(const Key('terminal-commit-reconcile')), findsNothing);
    await tester.pump(const Duration(milliseconds: 3200));
  });

  testWidgets('B2 commit unknown 且 ledger 无记录 → 横幅可重新对账，不盲重放', (tester) async {
    final sessions = _FakeSessions([_s('s0')]);
    final git = _FakeGit()
      ..commitResult = {'kind': 'unknown', 'clientOperationId': 'srv-op-1'};
    await _pump(tester, sessions: sessions, git: git);

    await tester.tap(find.byTooltip('提交'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('提交').last);
    await tester.pumpAndSettle();

    expect(git.commitCalls, 1);
    expect(find.byKey(const Key('terminal-commit-reconcile')), findsOneWidget);
    expect(find.text('提交结果未知，请重新对账。'), findsOneWidget);
    // unknown 相位提交按钮锁定。
    expect(_iconButton(tester, Icons.commit).onPressed, isNull);

    // 重新对账仍未知：横幅保留，commit 不重发。
    await tester.tap(find.byKey(const Key('terminal-commit-reconcile')));
    await tester.pumpAndSettle();
    expect(git.commitCalls, 1);
    expect(git.ledgerQueryIds.length, 2);
    expect(git.ledgerQueryIds[1], git.ledgerQueryIds.first);
    expect(find.byKey(const Key('terminal-commit-reconcile')), findsOneWidget);
  });

  testWidgets('B2 commit/merge 交叉互锁：commit 未对账时禁用 merge，对账后恢复', (tester) async {
    final sessions = _FakeSessions([_s('s0')]);
    final git = _FakeGit()
      ..commitResult = {'kind': 'unknown', 'clientOperationId': 'srv-op-1'}
      ..mergeResult = {'kind': 'succeeded', 'value': <String, dynamic>{}};
    await _pump(
      tester,
      sessions: sessions,
      git: git,
      worktreeInfo: const {'id': 'w1', 'name': 'w1', 'branch': 'feat/x', 'isMain': false},
    );
    expect(_iconButton(tester, Icons.merge_type).onPressed, isNotNull);

    await tester.tap(find.byTooltip('提交'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('提交').last);
    await tester.pumpAndSettle();

    expect(find.byKey(const Key('terminal-commit-reconcile')), findsOneWidget);
    expect(_iconButton(tester, Icons.commit).onPressed, isNull);
    // 交叉覆盖：commit 未对账期间 merge 也不得发起。
    expect(_iconButton(tester, Icons.merge_type).onPressed, isNull);

    // 对账确认成功回到 idle 后互锁解除，merge 恢复可点。
    git.ledgerScript = (operationId) => {
          'state': 'succeeded',
          'intent': {'kind': 'commit'},
        };
    await tester.tap(find.byKey(const Key('terminal-commit-reconcile')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('terminal-commit-reconcile')), findsNothing);
    expect(_iconButton(tester, Icons.merge_type).onPressed, isNotNull);
    await tester.pump(const Duration(milliseconds: 3200));
  });

  testWidgets('B2 commit/merge 交叉互锁：merge 未对账时禁用 commit，对账后恢复', (tester) async {
    final sessions = _FakeSessions([_s('s0')]);
    final git = _FakeGit()
      ..mergeResult = {'kind': 'unknown', 'clientOperationId': 'srv-op-m1'};
    await _pump(
      tester,
      sessions: sessions,
      git: git,
      worktreeInfo: const {'id': 'w1', 'name': 'w1', 'branch': 'feat/x', 'isMain': false},
    );

    await tester.tap(find.byTooltip('合并'));
    await tester.pumpAndSettle();
    await tester.tap(find.widgetWithText(FilledButton, '合并'));
    await tester.pumpAndSettle();

    expect(find.byKey(const Key('terminal-merge-reconcile')), findsOneWidget);
    expect(_iconButton(tester, Icons.merge_type).onPressed, isNull);
    // 交叉覆盖：merge 未对账期间 commit 也不得发起。
    expect(_iconButton(tester, Icons.commit).onPressed, isNull);

    // 对账确认成功回到 idle 后互锁解除，commit 恢复可点。
    git.ledgerScript = (operationId) => {
          'state': 'succeeded',
          'intent': {
            'kind': 'merge',
            'sourceWorktreeId': 'w1',
          },
        };
    await tester.tap(find.byKey(const Key('terminal-merge-reconcile')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('terminal-merge-reconcile')), findsNothing);
    expect(_iconButton(tester, Icons.commit).onPressed, isNotNull);
    await tester.pump(const Duration(milliseconds: 3200));
  });

  testWidgets('B2 merge unknown → 同 id 查 ledger 确认成功 → 合并成功', (tester) async {
    final sessions = _FakeSessions([_s('s0')]);
    final git = _FakeGit()
      ..mergeResult = {'kind': 'unknown', 'clientOperationId': 'srv-op-m1'}
      ..trees = const [
        {
          'id': 'w-main',
          'name': 'main',
          'branch': 'main',
          'isMain': true,
          'path': '/repo',
        },
      ]
      ..ledgerScript = (operationId) => {
            'state': 'succeeded',
            'intent': {
              'kind': 'merge',
              'sourceWorktreeId': 'w1',
            },
          };
    var mutated = 0;
    await _pump(
      tester,
      sessions: sessions,
      git: git,
      onWorktreesMutated: () => mutated += 1,
      worktreeInfo: const {'id': 'w1', 'name': 'w1', 'branch': 'feat/x', 'isMain': false},
    );

    await tester.tap(find.byTooltip('合并'));
    await tester.pumpAndSettle();
    await tester.tap(find.widgetWithText(FilledButton, '合并'));
    await tester.pumpAndSettle();

    expect(git.mergeCalls, 1);
    expect(git.ledgerQueryIds, [git.mergeOperationIds.first]);
    expect(mutated, 1);
    expect(find.text('合并成功'), findsOneWidget);
    await tester.pump(const Duration(milliseconds: 3200));
  });

  testWidgets('B12 Prompt 优化 workingDirectory 优先 worktreePath', (tester) async {
    final sessions = _FakeSessions([_s('s0')]);
    final prompts = _FakePrompts();
    await _pump(
      tester,
      sessions: sessions,
      prompts: prompts,
      worktreePath: '/repo/.worktrees/feat-x',
    );

    await tester.tap(find.byTooltip('Prompt 优化'));
    await tester.pumpAndSettle();
    await tester.enterText(find.byType(TextField).last, '帮我优化');
    await tester.tap(find.text('写入当前终端'));
    await tester.pumpAndSettle();

    expect(prompts.workingDirectories, ['/repo/.worktrees/feat-x']);
    await tester.pump(const Duration(milliseconds: 3200));
  });

  testWidgets('收藏 sheet：列表渲染、搜索过滤、选中后关闭', (tester) async {
    final sessions = _FakeSessions([_s('s0')]);
    final prompts = _FakePrompts(favorites: [
      const FavoritePrompt(id: '1', title: '修 bug', content: 'please fix the bug', tags: ['bug']),
      const FavoritePrompt(id: '2', title: '发版', content: 'deploy to prod', tags: []),
    ]);
    await _pump(tester, sessions: sessions, prompts: prompts);

    await tester.tap(find.byTooltip('收藏 Prompt'));
    await tester.pumpAndSettle();
    expect(find.text('修 bug'), findsOneWidget);
    expect(find.text('deploy to prod'), findsOneWidget);
    expect(find.text('全部'), findsOneWidget);
    expect(find.text('bug'), findsOneWidget);

    await tester.enterText(find.byType(TextField).last, 'deploy');
    await tester.pumpAndSettle();
    expect(find.text('deploy to prod'), findsOneWidget);
    expect(find.text('please fix the bug'), findsNothing);

    await tester.tap(find.text('发版'));
    await tester.pumpAndSettle();
    // 选中后 sheet 关闭（泵过退场动画）。
    expect(find.text('收藏的 Prompt'), findsNothing);
  });

  testWidgets('收藏 sheet：加载失败给错误与重试', (tester) async {
    final sessions = _FakeSessions([_s('s0')]);
    final prompts = _FakePrompts(error: Exception('网络错误'));
    await _pump(tester, sessions: sessions, prompts: prompts);

    await tester.tap(find.byTooltip('收藏 Prompt'));
    await tester.pumpAndSettle();
    expect(find.textContaining('加载收藏失败'), findsOneWidget);
    expect(find.text('重试'), findsOneWidget);
  });

  testWidgets('Prompt 优化：提交成功关闭对话框并提示已发送', (tester) async {
    final sessions = _FakeSessions([_s('s0')]);
    final prompts = _FakePrompts();
    await _pump(tester, sessions: sessions, prompts: prompts);

    await tester.tap(find.byTooltip('Prompt 优化'));
    await tester.pumpAndSettle();
    await tester.enterText(find.byType(TextField).last, '帮我优化这段 prompt');
    await tester.tap(find.text('写入当前终端'));
    await tester.pumpAndSettle();

    expect(prompts.optimized, ['帮我优化这段 prompt']);
    // worktreePath 未提供时回退 project.path（B12 回退分支）。
    expect(prompts.workingDirectories, ['/tmp/demo']);
    expect(find.text('已发送'), findsOneWidget);
    expect(find.text('Prompt 优化'), findsNothing);
    await tester.pump(const Duration(milliseconds: 3200));
  });

  testWidgets('boot 失败：错误页含文案与重试按钮，重试仍失败可再试，成功后正常进入', (tester) async {
    final sessions = _FailingListSessions([_s('s0')]);
    await _pump(tester, sessions: sessions);

    expect(find.textContaining('boot 失败'), findsOneWidget);
    expect(find.byKey(const Key('terminal-boot-retry')), findsOneWidget);
    expect(find.byType(InputChip), findsNothing);

    // 重试仍失败：错误页保留，重试入口仍在。
    await tester.tap(find.byKey(const Key('terminal-boot-retry')));
    await tester.pump();
    await tester.pump();
    expect(find.byKey(const Key('terminal-boot-retry')), findsOneWidget);

    // 修复后重试：重新执行 boot（list → 激活 → replay），错误页消失。
    sessions.listError = null;
    await tester.tap(find.byKey(const Key('terminal-boot-retry')));
    await tester.pump();
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 150));

    expect(find.textContaining('boot 失败'), findsNothing);
    expect(find.byKey(const Key('terminal-boot-retry')), findsNothing);
    expect(find.byType(InputChip), findsOneWidget);
    expect(sessions.replayIds, ['s0']);
  });

  testWidgets('replay 门闩：完成前输入行/发送/extra keys 全部无效，完成后恢复并经 WS 发出', (tester) async {
    final socket = _FakeSocket();
    final http = _StubWebSocketHttp(() => socket);
    final sessions = _GatedReplaySessions([_s('s0')]);
    await _pump(tester, sessions: sessions, http: http);

    // 初始 replay 挂起：门闩关闭且输入 WS 尚未建立 → 输入行与发送按钮禁用。
    expect(sessions.replayIds, ['s0']);
    expect(http.connectCalls, 0);
    var field = tester.widget<TextField>(find.byKey(const Key('terminal-input-field')));
    expect(field.enabled, false);
    var send = tester.widget<IconButton>(find.byKey(const Key('terminal-input-send')));
    expect(send.onPressed, isNull);

    // 门闩期间 extra keys 也应无效（onSend → _send 被门闩丢弃）。
    final writesWhileGated = socket.writeCount;
    await tester.tap(find.text('Esc'));
    await tester.pump();
    expect(socket.writeCount, writesWhileGated);

    // replay 完成（成功）→ 放行门闩 → 建立输入 WS → 输入恢复可用。
    sessions.initialReplay.complete({'sessionId': 's0', 'buffer': '', 'lastSeq': 0});
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 50));

    expect(http.connectCalls, 1);
    field = tester.widget<TextField>(find.byKey(const Key('terminal-input-field')));
    expect(field.enabled, true);
    send = tester.widget<IconButton>(find.byKey(const Key('terminal-input-send')));
    expect(send.onPressed, isNotNull);

    // 输入行回车经 WS 发出；空输入（裸回车）不发送。
    await tester.enterText(find.byKey(const Key('terminal-input-field')), 'ls');
    await tester.testTextInput.receiveAction(TextInputAction.done);
    await tester.pump();
    expect(socket.writeCount, greaterThan(writesWhileGated));
    final writesAfterInput = socket.writeCount;

    await tester.enterText(find.byKey(const Key('terminal-input-field')), '');
    await tester.testTextInput.receiveAction(TextInputAction.done);
    await tester.pump();
    expect(socket.writeCount, writesAfterInput);
  });

  testWidgets('replay 失败：门闩同样放行（对齐 web catch 分支也置 replayReady）', (tester) async {
    final socket = _FakeSocket();
    final http = _StubWebSocketHttp(() => socket);
    final sessions = _GatedReplaySessions([_s('s0')]);
    await _pump(tester, sessions: sessions, http: http);

    sessions.initialReplay.completeError(Exception('replay 失败'));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 50));

    final field = tester.widget<TextField>(find.byKey(const Key('terminal-input-field')));
    expect(field.enabled, true);
    expect(http.connectCalls, 1);
  });

  testWidgets('输入 WS 断开：输入行禁用（状态行文案保持既有说明）', (tester) async {
    final socket = _FakeSocket();
    final http = _StubWebSocketHttp(() => socket);
    final sessions = _GatedReplaySessions([_s('s0')]);
    await _pump(tester, sessions: sessions, http: http);

    sessions.initialReplay.complete({'sessionId': 's0', 'buffer': '', 'lastSeq': 0});
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 50));
    var field = tester.widget<TextField>(find.byKey(const Key('terminal-input-field')));
    expect(field.enabled, true);

    // 对端关闭 → 输入 WS onDone → 输入行禁用。
    socket.closePeer();
    await tester.pump();
    await tester.pump();
    field = tester.widget<TextField>(find.byKey(const Key('terminal-input-field')));
    expect(field.enabled, false);
    final send = tester.widget<IconButton>(find.byKey(const Key('terminal-input-send')));
    expect(send.onPressed, isNull);
  });

  testWidgets('输入行启用条件含 running：exited 权威状态禁用，恢复 running 重新可用', (tester) async {
    final socket = _FakeSocket();
    final http = _StubWebSocketHttp(() => socket);
    final sessions = _ExitedAfterCloseSessions([_s('s0'), _s('s1'), _s('s2')]);
    await _pump(tester, sessions: sessions, http: http);

    // s0 running + replay 门闩放行 + 输入 WS ready：输入行可用。
    expect(
      tester.widget<TextField>(find.byKey(const Key('terminal-input-field'))).enabled,
      isTrue,
    );

    // 关闭非当前会话 s2 触发 _refreshSessions；fake 权威列表把剩余会话标为
    // exited → 输入行 fail-closed 禁用（对齐 web status === 'running' 严格比较）。
    sessions.markExited = true;
    await tester.tap(find.byTooltip('关闭窗口').at(2));
    await tester.pump();
    await tester.pump();
    await tester.pump();
    expect(
      tester.widget<TextField>(find.byKey(const Key('terminal-input-field'))).enabled,
      isFalse,
    );
    expect(
      tester.widget<IconButton>(find.byKey(const Key('terminal-input-send'))).onPressed,
      isNull,
    );

    // 权威列表恢复 running（借关闭 s1 再触发一次刷新）→ 输入行重新可用。
    sessions
      ..markExited = false
      ..sessions = [_s('s0'), _s('s1')];
    await tester.tap(find.byTooltip('关闭窗口').last);
    await tester.pump();
    await tester.pump();
    await tester.pump();
    expect(
      tester.widget<TextField>(find.byKey(const Key('terminal-input-field'))).enabled,
      isTrue,
    );
  });
}
