import 'dart:async';
import 'dart:io';

import 'package:cc_partner_mobile/address_book/book.dart';
import 'package:cc_partner_mobile/automation/client.dart';
import 'package:cc_partner_mobile/core/lan_http.dart';
import 'package:cc_partner_mobile/projects/client.dart';
import 'package:cc_partner_mobile/ui/automation_page.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

/// 测试桩客户端：不改 pubspec，直接继承 AutomationClient 覆盖全部方法。
class _FakeAutomationClient extends AutomationClient {
  _FakeAutomationClient() : super(LanHttpClient(), 'http://127.0.0.1:1');

  final List<String> calls = [];
  String? lastCreateAction;
  Map<String, dynamic>? lastCreateBody;
  Completer<void>? createGate;

  // ---- 可配置注入（null 时走默认行为）----
  List<Map<String, dynamic>> Function(String projectId)? viewsOverride;
  Map<String, dynamic> Function(String projectId)? snapshotOverride;
  Object? snapshotError;
  String? ownerDeviceId;
  Map<String, dynamic>? ownerDevice;
  List<Map<String, dynamic>>? blockCreatedViews;
  Map<String, dynamic>? appendedView;
  List<Map<String, dynamic>> experiments = [
    {'id': 'e1', 'title': 'ab test', 'state': 'running', 'status': 'running'},
  ];

  // ---- 记录 ----
  String? lastReorderBlockId;
  List<String>? lastReorderIds;
  String? lastAppendBlockId;
  Map<String, dynamic>? lastCreateBlockBody;
  String? lastApprovedExperimentId;
  String? lastApprovedWinnerId;
  String? lastCancelledExperimentId;

  String titleOf(String id) => id == 'b1a' ? 'step one' : 'step two';

  Map<String, dynamic> _blockMember(
    String id,
    int blockIndex, {
    String workflowState = 'backlog',
    String runState = 'idle',
  }) =>
      {
        'origin': 'local',
        'task': {
          'id': id,
          'title': titleOf(id),
          'goal': 'g-$id',
          'acceptanceCriteria': 'a-$id',
          'workflowState': workflowState,
          'runState': runState,
          'blockId': 'blk1',
          'blockTitle': 'Login block',
          'blockIndex': blockIndex,
          'createdAt': '2026-09-01T00:00:00Z',
        },
      };

  Map<String, dynamic> get _localTask => {
        'id': 't1',
        'title': 'ship app',
        'goal': 'deliver parity',
        'acceptanceCriteria': 'tests pass',
        'workflowState': 'backlog',
        'runState': 'idle',
        'blockedReason': null,
      };

  Map<String, dynamic> get _remoteTask => {
        'id': 't2',
        'title': 'mirror task',
        'goal': 'sync',
        'acceptanceCriteria': 'mirrored',
        'workflowState': 'inProgress',
        'runState': 'running',
        'attemptPhase': 'streaming',
        'lastRuntimeMessage': 'streaming tokens',
        'claudeSessionId': 'claude-9',
        'transcriptPath': '/tmp/t2.jsonl',
        'worktreeId': 'wt-2',
        'sessionId': 'sess-2',
      };

  Map<String, dynamic> get _failedOutbox => {
        'id': 'o1',
        'status': 'failed',
        'deviceName': 'Office PC',
        'remoteProjectPath': '/tmp/remote',
        'requestJson': '{"title":"queued remote"}',
        'lastError': 'connection refused',
      };

  /// 完整 live 快照：覆盖 runtime 快照条全部字段。
  Map<String, dynamic> fullLiveSnapshot(String projectId) => {
        'projectId': projectId,
        'remoteStatus': 'live',
        'generatedAt': '2026-09-19T01:00:00Z',
        'latestTickAt': '2026-09-19T01:00:05Z',
        'slotsUsed': 2,
        'maxConcurrentTasks': 3,
        'runningTasks': [
          {'taskId': 'r1', 'title': 'ship app'},
        ],
        'retryingTasks': <dynamic>[],
        'latestError': 'runner timeout once',
        'recentEvents': [
          {'id': 'rev1', 'taskTitle': 'ship app', 'message': 'streaming tokens'},
        ],
      };

  @override
  Future<List<Map<String, dynamic>>> listViews(String projectId) async {
    calls.add('listViews');
    if (viewsOverride != null) return viewsOverride!(projectId);
    return [
      {'origin': 'local', 'task': _localTask},
      // 已创建的任务块在刷新后仍应出现在列表里。
      ...?blockCreatedViews,
      {
        'origin': 'remote',
        'deviceId': 'd2',
        'deviceName': 'Office PC',
        'task': _remoteTask,
      },
      {'origin': 'pendingRemote', 'item': _failedOutbox},
    ];
  }

  @override
  Future<List<Map<String, dynamic>>> listExperiments(String projectId) async {
    calls.add('experiments');
    return experiments;
  }

  @override
  Future<Map<String, dynamic>> runtimeSnapshot(String projectId) async {
    calls.add('snapshot');
    if (snapshotError != null) throw snapshotError!;
    if (snapshotOverride != null) return snapshotOverride!(projectId);
    return {'projectId': projectId, 'remoteStatus': 'local', 'slotsUsed': 0};
  }

  @override
  Future<Map<String, dynamic>> createTask({
    required String projectId,
    required String title,
    required String goal,
    required String acceptanceCriteria,
    required String clientRequestId,
    String createAction = 'backlog',
  }) async {
    calls.add('create:$createAction');
    lastCreateAction = createAction;
    lastCreateBody = {
      'projectId': projectId,
      'title': title,
      'goal': goal,
      'acceptanceCriteria': acceptanceCriteria,
      'clientRequestId': clientRequestId,
      'createAction': createAction,
    };
    if (createGate != null) {
      await createGate!.future;
    }
    return {
      'origin': 'local',
      'task': {'id': 't9', 'title': title, 'workflowState': 'backlog'},
    };
  }

  @override
  Future<Map<String, dynamic>> createBlock({
    required String projectId,
    required String title,
    required List<Map<String, String>> members,
    required String createAction,
    required String clientRequestId,
  }) async {
    calls.add('createBlock:$createAction');
    lastCreateAction = createAction;
    lastCreateBlockBody = {
      'projectId': projectId,
      'title': title,
      'members': members,
      'createAction': createAction,
      'clientRequestId': clientRequestId,
    };
    blockCreatedViews ??= [
      _blockMember('b1a', 0),
      _blockMember('b1b', 1),
    ];
    return {
      'block': {'id': 'blk1', 'title': title},
      'tasks': blockCreatedViews!,
    };
  }

  @override
  Future<Map<String, dynamic>> appendBlockMember({
    required String projectId,
    required String blockId,
    required String title,
    required String goal,
    required String acceptanceCriteria,
    required String clientRequestId,
  }) async {
    calls.add('appendBlock:$blockId:$title');
    lastAppendBlockId = blockId;
    return appendedView ?? _blockMember('b1c', 2);
  }

  @override
  Future<List<Map<String, dynamic>>> reorderBlockMembers({
    required String projectId,
    required String blockId,
    required List<String> orderedTaskIds,
    required String clientRequestId,
  }) async {
    calls.add('reorder:$blockId:${orderedTaskIds.join(',')}');
    lastReorderBlockId = blockId;
    lastReorderIds = orderedTaskIds;
    return [
      for (var i = 0; i < orderedTaskIds.length; i++)
        _blockMember(orderedTaskIds[i], i),
    ];
  }

  @override
  Future<Map<String, dynamic>> approveExperimentWinner(
    String experimentId,
    String winnerTaskId,
  ) async {
    calls.add('approve:$experimentId:$winnerTaskId');
    lastApprovedExperimentId = experimentId;
    lastApprovedWinnerId = winnerTaskId;
    // 与真实服务端一致：更新存储中的实验组，刷新后不再回到 needsDecision。
    _replaceExperiment(experimentId, {'id': experimentId, 'status': 'completed'});
    return {'id': experimentId, 'status': 'completed'};
  }

  @override
  Future<Map<String, dynamic>> cancelExperiment(String experimentId) async {
    calls.add('cancelExperiment:$experimentId');
    lastCancelledExperimentId = experimentId;
    _replaceExperiment(experimentId, {'id': experimentId, 'status': 'cancelled'});
    return {'id': experimentId, 'status': 'cancelled'};
  }

  /// 用动作结果替换 fake 存储中的实验组。
  void _replaceExperiment(String experimentId, Map<String, dynamic> updated) {
    experiments = [
      for (final item in experiments)
        item['id'] == experimentId ? updated : item,
    ];
  }

  @override
  Future<Map<String, dynamic>> completePrompt(
    String projectId,
    String prompt, {
    String? workingDirectory,
  }) async {
    calls.add('completePrompt');
    if (prompt == 'boom') {
      throw Exception('CLI missing');
    }
    return {'title': 'AI 标题', 'goal': 'AI 目标', 'acceptanceCriteria': 'AI 验收'};
  }

  @override
  Future<List<Map<String, dynamic>>> listEvidence(
    String projectId,
    String taskId,
  ) async {
    calls.add('evidence:$taskId');
    return [
      {
        'id': 'ev1',
        'taskId': taskId,
        'kind': 'verificationOutput',
        'title': 'verify',
        'summary': 'ok',
        'content': 'all green',
        'createdAt': '2026-09-19T10:00:00Z',
      },
    ];
  }

  @override
  Future<Map<String, dynamic>> retryOutbox(
    String projectId,
    String outboxId,
  ) async {
    calls.add('retry:$outboxId');
    return {'id': outboxId, 'status': 'pending'};
  }

  @override
  Future<Map<String, dynamic>> discardOutbox(
    String projectId,
    String outboxId,
  ) async {
    calls.add('discard:$outboxId');
    return {'id': outboxId, 'status': 'discarded'};
  }

  @override
  Future<String?> projectOwnerDeviceId(String projectId) async {
    calls.add('ownerDevice');
    return ownerDeviceId;
  }

  @override
  Future<List<Map<String, dynamic>>> listDevices() async {
    calls.add('devices');
    if (ownerDeviceId == null || ownerDevice == null) return const [];
    return [ownerDevice!];
  }
}

Future<void> _pumpPage(
  WidgetTester tester,
  _FakeAutomationClient client, {
  String? focusTaskId,
  String? focusOutboxId,
  void Function(String? worktreeId, String? sessionId)? onFocusSession,
  VoidCallback? onFocusMissing,
  VoidCallback? onExternalMutation,
  ProjectSummary? project,
}) async {
  await tester.pumpWidget(
    MaterialApp(
      home: Scaffold(
        body: AutomationPage(
          book: AddressBook(store: MemoryAddressBookStore()),
          http: LanHttpClient(),
          project:
              project ??
              const ProjectSummary(
                id: 'p1',
                name: 'demo',
                kind: 'local',
                path: '/repo/demo',
              ),
          focusTaskId: focusTaskId,
          focusOutboxId: focusOutboxId,
          onFocusSession: onFocusSession,
          onFocusMissing: onFocusMissing,
          onExternalMutation: onExternalMutation,
          clientOverride: client,
        ),
      ),
    ),
  );
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('swim lanes render non-empty groups and keep todo titled', (
    tester,
  ) async {
    final client = _FakeAutomationClient();
    await _pumpPage(tester, client);

    // 泳道标题：backlog/todo 恒显；其余只渲染非空泳道。
    expect(find.byKey(const Key('automation-lane-backlog')), findsOneWidget);
    expect(find.byKey(const Key('automation-lane-todo')), findsOneWidget);
    expect(find.byKey(const Key('automation-lane-inProgress')), findsOneWidget);
    expect(find.byKey(const Key('automation-lane-done')), findsNothing);

    // 任务行：标题 + 来源徽章 + workflow 徽章。
    expect(find.byKey(const Key('automation-task-t1')), findsOneWidget);
    expect(find.byKey(const Key('automation-task-t2')), findsOneWidget);
    expect(find.text('本机'), findsWidgets);
    expect(find.text('远端 Office PC'), findsOneWidget);
    expect(find.text('In Progress'), findsWidgets);

    // runtime 状态卡与离线 outbox 区。
    expect(find.text('运行时状态'), findsOneWidget);
    expect(find.text('本机'), findsWidgets);
    expect(find.byKey(const Key('automation-outbox-o1')), findsOneWidget);
    expect(find.text('queued remote'), findsOneWidget);
    expect(find.text('发送失败'), findsOneWidget);
    expect(find.text('目标设备：Office PC'), findsOneWidget);
    expect(find.text('发送错误：connection refused'), findsOneWidget);

    // 实验组区固定在页面底部（空列表也有文案）。
    await tester.scrollUntilVisible(
      find.text('实验组'),
      200,
      scrollable: find.byType(Scrollable).first,
    );
    await tester.pumpAndSettle();
    expect(find.text('实验组'), findsOneWidget);
    expect(find.text('ab test'), findsOneWidget);
  });

  testWidgets('runtime strip renders live snapshot fields', (tester) async {
    final client = _FakeAutomationClient();
    client.snapshotOverride = client.fullLiveSnapshot;
    await _pumpPage(tester, client);

    expect(
      find.descendant(
        of: find.byKey(const Key('automation-runtime-badge')),
        matching: find.text('在线'),
      ),
      findsOneWidget,
    );
    expect(find.textContaining('生成时间'), findsOneWidget);
    expect(find.textContaining('最近 tick'), findsOneWidget);
    expect(find.text('槽位 2/3 · 运行 1 · 重试 0'), findsOneWidget);
    expect(find.text('最近错误：runner timeout once'), findsOneWidget);
    expect(find.text('ship app: streaming tokens'), findsOneWidget);
    expect(find.byKey(const Key('automation-runtime-event-rev1')), findsOneWidget);
    // live 成功也有「最后更新」，但 warm 提示只在 offline 出现。
    expect(find.textContaining('最后更新'), findsOneWidget);
    expect(find.text('缓存仅用于显示，不启用任务动作'), findsNothing);
  });

  testWidgets('warm offline keeps cached snapshot and shows cache hint', (
    tester,
  ) async {
    final client = _FakeAutomationClient();
    client.snapshotOverride = client.fullLiveSnapshot;
    await _pumpPage(tester, client);
    expect(
      find.descendant(
        of: find.byKey(const Key('automation-runtime-badge')),
        matching: find.text('在线'),
      ),
      findsOneWidget,
    );

    // 网络类失败 + 已有 live 缓存 → offline warm。
    client.snapshotOverride = null;
    client.snapshotError = const SocketException('network down');
    await tester.tap(find.byKey(const Key('automation-refresh')));
    await tester.pumpAndSettle();

    expect(
      find.descendant(
        of: find.byKey(const Key('automation-runtime-badge')),
        matching: find.text('离线'),
      ),
      findsOneWidget,
    );
    expect(find.text('缓存仅用于显示，不启用任务动作'), findsOneWidget);
    expect(find.textContaining('生成时间'), findsOneWidget);
    expect(find.textContaining('最后更新'), findsOneWidget);
  });

  testWidgets('cold offline never claims cache', (tester) async {
    final client = _FakeAutomationClient()
      ..snapshotOverride = (projectId) => {
            'projectId': projectId,
            'remoteStatus': 'offline',
          };
    await _pumpPage(tester, client);
    expect(
      find.descendant(
        of: find.byKey(const Key('automation-runtime-badge')),
        matching: find.text('离线'),
      ),
      findsOneWidget,
    );
    expect(find.text('缓存仅用于显示，不启用任务动作'), findsNothing);
    expect(find.textContaining('最后更新'), findsNothing);
    expect(find.textContaining('生成时间'), findsNothing);
  });

  testWidgets('unsupported status shows dedicated badge without snapshot', (
    tester,
  ) async {
    final client = _FakeAutomationClient()
      ..snapshotOverride = (projectId) => {
            'projectId': projectId,
            'remoteStatus': 'unsupported',
          };
    await _pumpPage(tester, client);
    expect(
      find.descendant(
        of: find.byKey(const Key('automation-runtime-badge')),
        matching: find.text('对端不支持'),
      ),
      findsOneWidget,
    );
    expect(find.textContaining('生成时间'), findsNothing);
  });

  testWidgets('block group renders members with reorder and append entries', (
    tester,
  ) async {
    final client = _FakeAutomationClient();
    client.viewsOverride =
        (projectId) => [
              client._blockMember('b1a', 0),
              client._blockMember('b1b', 1),
            ];
    await _pumpPage(tester, client);

    // 块卡片落在 head 泳道，块标题 + 成员数徽章。
    expect(find.byKey(const Key('automation-block-blk1')), findsOneWidget);
    expect(find.text('Login block'), findsOneWidget);
    expect(find.text('2 步'), findsOneWidget);
    // 未展开时不渲染成员与操作。
    expect(find.byKey(const Key('automation-task-b1a')), findsNothing);
    expect(find.byKey(const Key('block-append-blk1')), findsNothing);

    await tester.tap(find.byKey(const Key('automation-block-blk1')));
    await tester.pumpAndSettle();
    expect(find.text('step one'), findsOneWidget);
    expect(find.text('step two'), findsOneWidget);
    expect(find.byKey(const Key('block-move-up-blk1-0')), findsOneWidget);
    expect(find.byKey(const Key('block-move-down-blk1-1')), findsOneWidget);

    // 上移第二个成员：交换相邻后提交完整置换。
    await tester.tap(find.byKey(const Key('block-move-up-blk1-1')));
    await tester.pumpAndSettle();
    expect(client.lastReorderBlockId, 'blk1');
    expect(client.lastReorderIds, ['b1b', 'b1a']);
    // 返回的最新成员视图被 upsert，块内顺序变化。
    expect(find.text('step one'), findsOneWidget);
    expect(find.text('step two'), findsOneWidget);
    expect(find.byKey(const Key('block-append-blk1')), findsOneWidget);
  });

  testWidgets('block append dialog submits three fields to the block', (
    tester,
  ) async {
    final client = _FakeAutomationClient();
    client.viewsOverride =
        (projectId) => [
              client._blockMember('b1a', 0),
              client._blockMember('b1b', 1),
            ];
    await _pumpPage(tester, client);

    await tester.tap(find.byKey(const Key('automation-block-blk1')));
    await tester.pumpAndSettle();
    // 追加按钮在展开内容末尾，滚动到可见后再点。
    await tester.ensureVisible(find.byKey(const Key('block-append-blk1')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('block-append-blk1')));
    await tester.pumpAndSettle();
    // 对话框标题与块内追加按钮同名，共 2 处。
    expect(find.text('在末尾添加任务'), findsNWidgets(2));
    // 追加模式没有块模式切换与动作选择。
    expect(find.byKey(const Key('create-mode')), findsNothing);
    expect(find.byKey(const Key('create-actions')), findsNothing);

    await tester.enterText(find.byKey(const Key('create-title')), 'step three');
    await tester.enterText(find.byKey(const Key('create-goal')), 'g3');
    await tester.enterText(find.byKey(const Key('create-acceptance')), 'a3');
    await tester.tap(find.byKey(const Key('append-submit')));
    await tester.pumpAndSettle();

    expect(client.lastAppendBlockId, 'blk1');
    expect(client.calls, contains('appendBlock:blk1:step three'));
    expect(find.byKey(const Key('append-submit')), findsNothing);
  });

  testWidgets('create dialog block mode submits one create-block call', (
    tester,
  ) async {
    final client = _FakeAutomationClient();
    await _pumpPage(tester, client);

    await tester.tap(find.byKey(const Key('automation-create')));
    await tester.pumpAndSettle();
    // 切到任务块模式。
    await tester.tap(find.text('任务块'));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('block-title')), findsOneWidget);
    expect(find.byKey(const Key('block-member-title-0')), findsOneWidget);
    expect(find.byKey(const Key('block-member-title-1')), findsOneWidget);
    // 默认 2 个成员，最多 8 个：滚动到「添加成员」加到第 3 个，再删回 2 个。
    await tester.ensureVisible(find.byKey(const Key('block-add-member')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('block-add-member')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('block-member-title-2')), findsOneWidget);
    await tester.ensureVisible(find.byKey(const Key('block-member-remove-2')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('block-member-remove-2')));
    await tester.pumpAndSettle();

    await tester.enterText(find.byKey(const Key('block-title')), 'Login block');
    await tester.enterText(
      find.byKey(const Key('block-member-title-0')),
      'step one',
    );
    await tester.enterText(find.byKey(const Key('block-member-goal-0')), 'g1');
    await tester.enterText(
      find.byKey(const Key('block-member-acceptance-0')),
      'a1',
    );
    await tester.enterText(
      find.byKey(const Key('block-member-title-1')),
      'step two',
    );
    await tester.enterText(find.byKey(const Key('block-member-goal-1')), 'g2');
    await tester.enterText(
      find.byKey(const Key('block-member-acceptance-1')),
      'a2',
    );
    await tester.ensureVisible(find.text('直接开始'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('直接开始'));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('create-submit')));
    await tester.pumpAndSettle();

    expect(client.lastCreateAction, 'start');
    expect(client.lastCreateBlockBody?['title'], 'Login block');
    expect(client.calls, contains('createBlock:start'));
    expect(
      (client.lastCreateBlockBody?['members'] as List).length,
      2,
    );
    expect(find.text('任务已创建并启动'), findsOneWidget);
    expect(find.byKey(const Key('create-submit')), findsNothing);
    // 新块立即可见。
    expect(find.text('Login block'), findsOneWidget);
    expect(find.text('2 步'), findsOneWidget);
  });

  testWidgets('block creation gated by owner peer capability for remote', (
    tester,
  ) async {
    final client = _FakeAutomationClient()
      ..ownerDeviceId = 'd2'
      ..ownerDevice = const {
        'id': 'd2',
        'protoVersion': 0,
        'capabilities': <String>[],
      };
    final remote = const ProjectSummary(id: 'p1', name: 'demo', kind: 'remote');
    await _pumpPage(tester, client, project: remote);

    // 能力不支持：泳道「+块」禁用，对话框内出现 assist 文案且切不过去。
    expect(
      tester
          .widget<TextButton>(
            find.byKey(const Key('automation-lane-add-block-backlog')),
          )
          .onPressed,
      isNull,
    );
    await tester.tap(find.byKey(const Key('automation-create')));
    await tester.pumpAndSettle();
    expect(find.text('对端不支持任务块。'), findsOneWidget);
    await tester.tap(find.text('任务块'));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('block-title')), findsNothing);
    await tester.tap(find.text('取消'));
    await tester.pumpAndSettle();

    // owner 支持 task-blocks 能力后入口恢复（换项目 id 触发页面按新项目重置）。
    final capable = _FakeAutomationClient()
      ..ownerDeviceId = 'd2'
      ..ownerDevice = const {
        'id': 'd2',
        'protoVersion': 1,
        'capabilities': ['orchestrator.task-blocks.v1'],
      };
    await _pumpPage(
      tester,
      capable,
      project: const ProjectSummary(id: 'p2', name: 'demo', kind: 'remote'),
    );
    expect(
      tester
          .widget<TextButton>(
            find.byKey(const Key('automation-lane-add-block-backlog')),
          )
          .onPressed,
      isNotNull,
    );
  });

  testWidgets('lane block entry opens block mode with lane preferred action', (
    tester,
  ) async {
    final client = _FakeAutomationClient();
    await _pumpPage(tester, client);
    await tester.tap(find.byKey(const Key('automation-lane-add-block-todo')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('block-title')), findsOneWidget);
  });

  testWidgets('detail shows runtime fields and execution context actions', (
    tester,
  ) async {
    final client = _FakeAutomationClient();
    String? focusedWorktree;
    String? focusedSession;
    await _pumpPage(
      tester,
      client,
      onFocusSession: (worktreeId, sessionId) {
        focusedWorktree = worktreeId;
        focusedSession = sessionId;
      },
    );

    // 无 worktree/session 的任务：按钮禁用 + 说明文案。
    await tester.tap(find.byKey(const Key('automation-task-t1')));
    await tester.pumpAndSettle();
    expect(find.text('运行消息'), findsOneWidget);
    expect(find.text('Claude Session'), findsOneWidget);
    expect(find.text('Transcript'), findsOneWidget);
    expect(find.text('unknown'), findsWidgets);
    final disabled = tester.widget<FilledButton>(
      find.byKey(const Key('automation-open-execution')),
    );
    expect(disabled.onPressed, isNull);
    expect(find.text('暂无执行现场'), findsOneWidget);
    expect(find.text('关闭详情'), findsOneWidget);
    await tester.ensureVisible(find.text('关闭详情'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('关闭详情'));
    await tester.pumpAndSettle();

    // 有 worktree+session 的任务：按钮启用并回调壳层。
    await tester.ensureVisible(find.byKey(const Key('automation-task-t2')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('automation-task-t2')));
    await tester.pumpAndSettle();
    expect(find.text('streaming tokens'), findsOneWidget);
    expect(find.text('claude-9'), findsOneWidget);
    final enabled = tester.widget<FilledButton>(
      find.byKey(const Key('automation-open-execution')),
    );
    expect(enabled.onPressed, isNotNull);
    await tester.ensureVisible(find.byKey(const Key('automation-open-execution')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('automation-open-execution')));
    await tester.pumpAndSettle();
    expect(focusedWorktree, 'wt-2');
    expect(focusedSession, 'sess-2');
  });

  testWidgets('single-sided worktree or session still enables execution focus', (
    tester,
  ) async {
    final client = _FakeAutomationClient()
      ..viewsOverride = (projectId) => [
            {
              'origin': 'local',
              'task': {
                'id': 't-wt',
                'title': 'only worktree',
                'worktreeId': 'wt-only',
              },
            },
            {
              'origin': 'local',
              'task': {
                'id': 't-sess',
                'title': 'only session',
                'sessionId': 'sess-only',
              },
            },
          ];
    String? focusedWorktree;
    String? focusedSession;
    await _pumpPage(
      tester,
      client,
      onFocusSession: (worktreeId, sessionId) {
        focusedWorktree = worktreeId;
        focusedSession = sessionId;
      },
    );

    // 只有 worktreeId：按钮启用，session 以 null 原样回调（壳层回落）。
    await tester.tap(find.byKey(const Key('automation-task-t-wt')));
    await tester.pumpAndSettle();
    final wtButton = tester.widget<FilledButton>(
      find.byKey(const Key('automation-open-execution')),
    );
    expect(wtButton.onPressed, isNotNull);
    expect(find.text('打开执行现场'), findsOneWidget);
    await tester.ensureVisible(find.byKey(const Key('automation-open-execution')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('automation-open-execution')));
    await tester.pumpAndSettle();
    expect(focusedWorktree, 'wt-only');
    expect(focusedSession, isNull);

    // 只有 sessionId：同样启用，worktree 以 null 原样回调。
    await tester.ensureVisible(find.byKey(const Key('automation-task-t-sess')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('automation-task-t-sess')));
    await tester.pumpAndSettle();
    final sessButton = tester.widget<FilledButton>(
      find.byKey(const Key('automation-open-execution')),
    );
    expect(sessButton.onPressed, isNotNull);
    await tester.ensureVisible(find.byKey(const Key('automation-open-execution')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('automation-open-execution')));
    await tester.pumpAndSettle();
    expect(focusedWorktree, isNull);
    expect(focusedSession, 'sess-only');
  });

  testWidgets('experiments support approve and cancel with inbox notice', (
    tester,
  ) async {
    final client = _FakeAutomationClient()
      ..experiments = [
        {
          'id': 'e1',
          'title': 'provider duel',
          'status': 'needsDecision',
          'goal': 'pick a winner',
          'selectionReason': 'faster p95',
          'winnerTaskId': 'cand-2',
          'candidates': [
            {
              'taskId': 'cand-1',
              'ordinal': 1,
              'providerId': 'codex',
              'strategyLabel': 'baseline',
              'outcome': 'candidateReady',
            },
            {
              'taskId': 'cand-2',
              'ordinal': 2,
              'providerId': 'claude',
              'strategyLabel': 'fast',
              'outcome': 'winner',
            },
          ],
        },
      ];
    var mutations = 0;
    await _pumpPage(
      tester,
      client,
      onExternalMutation: () => mutations++,
    );

    // 实验组区在页面底部：先滚动到按钮进入树，再 ensureVisible 后点按。
    const approveFinder = Key('automation-experiment-approve-e1');
    await tester.scrollUntilVisible(
      find.byKey(approveFinder),
      200,
      scrollable: find.byType(Scrollable).first,
    );
    await tester.pumpAndSettle();
    await tester.ensureVisible(find.byKey(approveFinder));
    await tester.pumpAndSettle();
    // candidate 与推荐理由可见；不展示 diff。
    expect(find.text('provider duel'), findsOneWidget);
    expect(find.text('faster p95'), findsOneWidget);
    expect(find.textContaining('codex'), findsOneWidget);

    await tester.tap(find.byKey(approveFinder));
    await tester.pumpAndSettle();
    expect(client.lastApprovedExperimentId, 'e1');
    expect(client.lastApprovedWinnerId, 'cand-2');
    expect(find.text('已采纳推荐结果'), findsOneWidget);
    expect(mutations, 1);
    // 决策完成后 needsDecision 动作区消失（对齐 web）。
    expect(find.byKey(approveFinder), findsNothing);
  });

  testWidgets('experiments cancel calls the route with inbox notice', (
    tester,
  ) async {
    final client = _FakeAutomationClient()
      ..experiments = [
        {
          'id': 'e1',
          'title': 'provider duel',
          'status': 'needsDecision',
          'goal': 'pick a winner',
          'winnerTaskId': 'cand-2',
        },
      ];
    var mutations = 0;
    await _pumpPage(
      tester,
      client,
      onExternalMutation: () => mutations++,
    );

    const cancelFinder = Key('automation-experiment-cancel-e1');
    await tester.scrollUntilVisible(
      find.byKey(cancelFinder),
      200,
      scrollable: find.byType(Scrollable).first,
    );
    await tester.pumpAndSettle();
    await tester.ensureVisible(find.byKey(cancelFinder));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(cancelFinder));
    await tester.pumpAndSettle();
    expect(client.lastCancelledExperimentId, 'e1');
    expect(find.text('已取消实验组'), findsOneWidget);
    expect(mutations, 1);
  });

  testWidgets('attention focus selects task detail when found', (tester) async {
    final client = _FakeAutomationClient();
    await _pumpPage(tester, client, focusTaskId: 't1');
    expect(find.byKey(const Key('automation-detail')), findsOneWidget);
    expect(client.calls, contains('evidence:t1'));
  });

  testWidgets('attention focus reports missing ids to the shell', (
    tester,
  ) async {
    final client = _FakeAutomationClient();
    var missing = 0;
    await _pumpPage(
      tester,
      client,
      focusTaskId: 'nope',
      onFocusMissing: () => missing++,
    );
    expect(missing, 1);
    expect(find.byKey(const Key('automation-detail')), findsNothing);

    final outboxClient = _FakeAutomationClient();
    var outboxMissing = 0;
    await _pumpPage(
      tester,
      outboxClient,
      focusOutboxId: 'gone',
      onFocusMissing: () => outboxMissing++,
    );
    expect(outboxMissing, 1);
  });

  testWidgets('attention focus highlights the matched outbox row', (
    tester,
  ) async {
    final client = _FakeAutomationClient();
    await _pumpPage(tester, client, focusOutboxId: 'o1');
    final card = tester.widget<Card>(
      find.byKey(const Key('automation-outbox-o1')),
    );
    expect(card.color, isNotNull);
    // 未聚焦的任务卡保持默认底色。
    final plain = tester.widget<Card>(
      find.byKey(const Key('automation-task-t1')),
    );
    expect(plain.color, isNull);
  });

  testWidgets('task row expands detail with evidence timeline', (tester) async {
    final client = _FakeAutomationClient();
    await _pumpPage(tester, client);

    await tester.tap(find.byKey(const Key('automation-task-t1')));
    await tester.pumpAndSettle();
    expect(client.calls, contains('evidence:t1'));
    expect(find.byKey(const Key('automation-detail')), findsOneWidget);
    expect(find.text('验收标准'), findsOneWidget);
    expect(find.text('阻塞原因'), findsNothing);
    expect(find.byKey(const Key('automation-evidence-ev1')), findsOneWidget);
    expect(find.text('验证输出'), findsOneWidget);

    // content 可展开。
    expect(find.text('all green'), findsNothing);
    await tester.ensureVisible(
      find.byKey(const Key('automation-evidence-expand-ev1')),
    );
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('automation-evidence-expand-ev1')));
    await tester.pumpAndSettle();
    expect(find.text('all green'), findsOneWidget);

    // 关闭详情（详情较长，先滚动到按钮可见）。
    await tester.ensureVisible(find.text('关闭详情'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('关闭详情'));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('automation-detail')), findsNothing);
  });

  testWidgets('create dialog submits with the picked action and refreshes', (
    tester,
  ) async {
    final client = _FakeAutomationClient();
    await _pumpPage(tester, client);

    await tester.tap(find.byKey(const Key('automation-create')));
    await tester.pumpAndSettle();
    await tester.enterText(
      find.byKey(const Key('create-title')),
      'cover browser',
    );
    await tester.enterText(find.byKey(const Key('create-goal')), 'ship it');
    await tester.enterText(
      find.byKey(const Key('create-acceptance')),
      'tests pass',
    );

    // 默认动作 backlog；切换到 直接开始（对话框较高，先滚动到动作区可见）。
    await tester.ensureVisible(find.text('直接开始'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('直接开始'));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('create-submit')));
    await tester.pumpAndSettle();

    expect(client.lastCreateAction, 'start');
    expect(client.lastCreateBody?['title'], 'cover browser');
    expect(client.calls.where((c) => c.startsWith('create:')), ['create:start']);
    expect(find.text('任务已创建并启动'), findsOneWidget);
    // 对话框已关闭（提交按钮不再存在）。
    expect(find.byKey(const Key('create-submit')), findsNothing);
    // 成功后列表刷新。
    expect(client.calls.where((c) => c == 'listViews').length, greaterThan(1));
  });

  testWidgets('AI completion fills the form and failure asks manual input', (
    tester,
  ) async {
    final client = _FakeAutomationClient();
    await _pumpPage(tester, client);

    await tester.tap(find.byKey(const Key('automation-create')));
    await tester.pumpAndSettle();
    await tester.enterText(find.byKey(const Key('create-prompt')), '做个预览');
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('create-ai')));
    await tester.pumpAndSettle();

    expect(find.byKey(const Key('create-title')), findsOneWidget);
    expect(find.text('AI 标题'), findsOneWidget);
    expect(find.text('AI 目标'), findsOneWidget);
    expect(find.text('AI 验收'), findsOneWidget);

    // 失败路径：SnackBar 提示手填。
    await tester.enterText(find.byKey(const Key('create-prompt')), 'boom');
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('create-ai')));
    await tester.pumpAndSettle();
    expect(find.textContaining('AI 完善失败，请手动填写'), findsOneWidget);
  });

  testWidgets('create dialog locks close while submitting', (tester) async {
    final client = _FakeAutomationClient();
    final gate = Completer<void>();
    client.createGate = gate;
    await _pumpPage(tester, client);

    await tester.tap(find.byKey(const Key('automation-create')));
    await tester.pumpAndSettle();
    await tester.enterText(find.byKey(const Key('create-title')), 't');
    await tester.enterText(find.byKey(const Key('create-goal')), 'g');
    await tester.enterText(find.byKey(const Key('create-acceptance')), 'a');
    await tester.tap(find.byKey(const Key('create-submit')));
    await tester.pump();

    // busy：按钮显示创建中且禁用，取消按钮禁用，对话框不可关闭。
    expect(find.text('创建中…'), findsOneWidget);
    final cancelFinder = find.widgetWithText(TextButton, '取消');
    expect(tester.widget<TextButton>(cancelFinder).onPressed, isNull);
    expect(
      tester.widget<FilledButton>(
        find.byKey(const Key('create-submit')),
      ).onPressed,
      isNull,
    );

    gate.complete();
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('create-submit')), findsNothing);
  });

  testWidgets('outbox discard requires confirmation before calling the route', (
    tester,
  ) async {
    final client = _FakeAutomationClient();
    await _pumpPage(tester, client);
    final listScrollable = find.byType(Scrollable).first;

    // 第一次点丢弃：只弹确认框，不调用路由。
    await tester.scrollUntilVisible(
      find.byKey(const Key('automation-outbox-discard-o1')),
      200,
      scrollable: listScrollable,
    );
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('automation-outbox-discard-o1')));
    await tester.pumpAndSettle();
    expect(find.text('确定丢弃这条失败的离线发送请求？'), findsOneWidget);
    expect(client.calls, isNot(contains('discard:o1')));

    // 取消：不丢弃。
    await tester.tap(find.text('取消'));
    await tester.pumpAndSettle();
    expect(client.calls, isNot(contains('discard:o1')));

    // 再点丢弃并确认：调用 discard 路由并提示成功。
    await tester.tap(find.byKey(const Key('automation-outbox-discard-o1')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('outbox-discard-confirm')));
    await tester.pumpAndSettle();
    expect(client.calls, contains('discard:o1'));
    expect(find.text('已丢弃该离线任务'), findsOneWidget);
  });

  testWidgets('outbox retry hits the retry route and shows a snackbar', (
    tester,
  ) async {
    final client = _FakeAutomationClient();
    await _pumpPage(tester, client);

    await tester.scrollUntilVisible(
      find.byKey(const Key('automation-outbox-retry-o1')),
      200,
      scrollable: find.byType(Scrollable).first,
    );
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('automation-outbox-retry-o1')));
    await tester.pumpAndSettle();
    expect(client.calls, contains('retry:o1'));
    expect(find.text('已重新加入发送队列'), findsOneWidget);
  });
}
