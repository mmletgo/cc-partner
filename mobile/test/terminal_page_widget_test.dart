import 'package:cc_partner_mobile/address_book/book.dart';
import 'package:cc_partner_mobile/address_book/models.dart';
import 'package:cc_partner_mobile/core/lan_http.dart';
import 'package:cc_partner_mobile/git/client.dart';
import 'package:cc_partner_mobile/prompts/client.dart';
import 'package:cc_partner_mobile/projects/client.dart';
import 'package:cc_partner_mobile/sessions/client.dart';
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

AddressBook _book() {
  final book = AddressBook(store: MemoryAddressBookStore());
  book.servers.add(ServerRecord(
    id: 'pc',
    host: '127.0.0.1',
    port: 1,
    baseUrl: 'http://127.0.0.1:1',
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

class _FakeGit extends GitClient {
  _FakeGit() : super(LanHttpClient(), 'http://127.0.0.1:1');

  Map<String, dynamic> commitResult = {'kind': 'succeeded', 'value': <String, dynamic>{}};
  Map<String, dynamic> mergeResult = {'kind': 'succeeded', 'value': <String, dynamic>{}};
  Map<String, dynamic> repairResult = {'terminalSessionId': 's-repair'};
  int commitCalls = 0;
  int mergeCalls = 0;
  int repairCalls = 0;
  String? lastCommitMessage;

  @override
  Future<Map<String, dynamic>> commit({
    required String worktreeId,
    required String clientOperationId,
    String? message,
  }) async {
    commitCalls += 1;
    lastCommitMessage = message;
    return commitResult;
  }

  @override
  Future<Map<String, dynamic>> merge({
    required String projectId,
    required String worktreeId,
    required String clientOperationId,
  }) async {
    mergeCalls += 1;
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
}

class _FakePrompts extends PromptsClient {
  _FakePrompts({this.favorites, this.error}) : super(LanHttpClient(), 'http://127.0.0.1:1');

  List<FavoritePrompt>? favorites;
  Object? error;
  final List<String> optimized = [];

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
    return <String, dynamic>{};
  }
}

ProjectSummary get _project => const ProjectSummary(id: 'p1', name: 'demo', path: '/tmp/demo');

Future<void> _pump(
  WidgetTester tester, {
  required SessionsClient sessions,
  PromptsClient? prompts,
  GitClient? git,
  ValueChanged<bool>? onFullscreenChanged,
  VoidCallback? onWorktreesMutated,
}) async {
  await tester.pumpWidget(MaterialApp(
    home: Scaffold(
      body: TerminalPage(
        book: _book(),
        http: LanHttpClient(),
        project: _project,
        worktreeId: 'w1',
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
    );

    await tester.tap(find.byTooltip('合并'));
    await tester.pumpAndSettle();
    expect(find.text('确定把“w1”合并到主工作区？'), findsOneWidget);

    await tester.tap(find.widgetWithText(FilledButton, '合并'));
    await tester.pumpAndSettle();
    expect(git.mergeCalls, 1);
    expect(mutated, 1);
    expect(find.text('合并成功'), findsOneWidget);
    // 让 SnackBar 的自动消失 Timer 走完，避免测试结束残留 pending Timer。
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
    expect(find.text('已发送'), findsOneWidget);
    expect(find.text('Prompt 优化'), findsNothing);
    await tester.pump(const Duration(milliseconds: 3200));
  });
}
