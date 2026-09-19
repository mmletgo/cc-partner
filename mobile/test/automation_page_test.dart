import 'dart:async';

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
      };

  Map<String, dynamic> get _failedOutbox => {
        'id': 'o1',
        'status': 'failed',
        'deviceName': 'Office PC',
        'remoteProjectPath': '/tmp/remote',
        'requestJson': '{"title":"queued remote"}',
        'lastError': 'connection refused',
      };

  @override
  Future<List<Map<String, dynamic>>> listViews(String projectId) async {
    calls.add('listViews');
    return [
      {'origin': 'local', 'task': _localTask},
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
    return [
      {'id': 'e1', 'title': 'ab test', 'state': 'running'},
    ];
  }

  @override
  Future<Map<String, dynamic>> runtimeSnapshot(String projectId) async {
    return {'remoteStatus': 'local', 'slotsUsed': 0};
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
}

Future<void> _pumpPage(
  WidgetTester tester,
  _FakeAutomationClient client,
) async {
  await tester.pumpWidget(
    MaterialApp(
      home: Scaffold(
        body: AutomationPage(
          book: AddressBook(store: MemoryAddressBookStore()),
          http: LanHttpClient(),
          project: const ProjectSummary(
            id: 'p1',
            name: 'demo',
            kind: 'local',
            path: '/repo/demo',
          ),
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

    // experiments 区保留在底部（需滚动到可见区域）。
    await tester.scrollUntilVisible(
      find.text('experiments'),
      200,
      scrollable: find.byType(Scrollable).first,
    );
    await tester.pumpAndSettle();
    expect(find.text('experiments'), findsOneWidget);
    expect(find.text('ab test'), findsOneWidget);
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
    await tester.tap(find.byKey(const Key('automation-evidence-expand-ev1')));
    await tester.pumpAndSettle();
    expect(find.text('all green'), findsOneWidget);

    // 关闭详情（详情较长，先滚动到按钮可见）。
    await tester.scrollUntilVisible(
      find.text('关闭详情'),
      200,
      scrollable: find.byType(Scrollable).first,
    );
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

    // 默认动作 backlog；切换到 直接开始。
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
