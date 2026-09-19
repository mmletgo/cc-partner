import 'dart:convert';
import 'dart:io';

import 'package:cc_partner_mobile/automation/client.dart';
import 'package:cc_partner_mobile/core/lan_http.dart';
import 'package:test/test.dart';

void main() {
  late HttpServer server;
  late String baseUrl;
  final captured = <String, Map<String, dynamic>>{};

  setUp(() async {
    captured.clear();
    server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    baseUrl = 'http://127.0.0.1:${server.port}';
    server.listen((request) async {
      final raw = await utf8.decodeStream(request);
      Map<String, dynamic> payload = const {};
      if (raw.isNotEmpty) {
        payload = jsonDecode(raw) as Map<String, dynamic>;
      }
      captured[request.uri.path] = payload;
      request.response.headers.contentType = ContentType.json;
      if (request.uri.path.endsWith('/task-views/list')) {
        request.response.write(
          jsonEncode({
            'views': [
              {
                'origin': 'local',
                'task': {
                  'id': 't1',
                  'title': 'ship app',
                  'goal': 'deliver',
                  'workflowState': 'todo',
                  'projectId': payload['projectId'],
                },
              },
              {
                'origin': 'remote',
                'deviceId': 'd2',
                'deviceName': 'Office PC',
                'task': {
                  'id': 't2',
                  'title': 'mirror task',
                  'goal': 'sync',
                  'workflowState': 'inProgress',
                },
              },
              {
                'origin': 'pendingRemote',
                'item': {
                  'id': 'o1',
                  'status': 'failed',
                  'deviceName': 'Office PC',
                  'remoteProjectPath': '/tmp/remote',
                  'requestJson': jsonEncode({'title': 'queued remote'}),
                  'lastError': 'connection refused',
                },
              },
            ],
          }),
        );
      } else if (request.uri.path.endsWith('/task-views/create')) {
        request.response.write(
          jsonEncode({
            'origin': 'local',
            'task': {'id': 't2', 'title': payload['title'], 'goal': payload['goal']},
          }),
        );
      } else if (request.uri.path.endsWith('/tasks/list')) {
        request.response.write(
          jsonEncode({
            'tasks': [
              {'id': 't1', 'title': 'ship app', 'projectId': payload['projectId']},
            ],
          }),
        );
      } else if (request.uri.path.endsWith('/complete-prompt')) {
        request.response.write(
          jsonEncode({
            'title': 'AI 标题',
            'goal': 'AI 目标',
            'acceptanceCriteria': 'AI 验收',
          }),
        );
      } else if (request.uri.path.endsWith('/tasks/evidence')) {
        request.response.write(
          jsonEncode({
            'evidence': [
              {
                'id': 'ev1',
                'taskId': payload['taskId'],
                'kind': 'verificationOutput',
                'title': 'verify',
                'summary': 'ok',
                'content': 'all green',
                'createdAt': '2026-09-19T10:00:00Z',
              },
            ],
          }),
        );
      } else if (request.uri.path.endsWith('/outbox/retry')) {
        request.response.write(
          jsonEncode({'id': payload['outboxId'], 'status': 'pending'}),
        );
      } else if (request.uri.path.endsWith('/outbox/discard')) {
        request.response.write(
          jsonEncode({'id': payload['outboxId'], 'status': 'discarded'}),
        );
      } else if (request.uri.path.endsWith('/experiments/list')) {
        request.response.write(
          jsonEncode({
            'experiments': [
              {'id': 'e1', 'title': 'ab'},
            ],
          }),
        );
      } else if (request.uri.path.endsWith('/runtime-snapshot')) {
        request.response.write(
          jsonEncode({'remoteStatus': 'local', 'slotsUsed': 0}),
        );
      } else {
        request.response.write('{}');
      }
      await request.response.close();
    });
  });

  tearDown(() async {
    await server.close(force: true);
  });

  test('lists swim-lane views and splits pending outbox items', () async {
    final http = LanHttpClient();
    addTearDown(http.close);
    final client = AutomationClient(http, baseUrl);
    final views = await client.listViews('p1');
    expect(views, hasLength(3));
    expect(captured['/api/orchestrator/task-views/list']?['projectId'], 'p1');
    final split = automationSplitViews(views);
    expect(split.tasks, hasLength(2));
    expect(automationTaskOfView(split.tasks.first)?['workflowState'], 'todo');
    expect(split.pendingRemoteItems.single['id'], 'o1');
    final groups = automationGroupByWorkflow(split.tasks);
    expect(groups['todo'], hasLength(1));
    expect(groups['inProgress'], hasLength(1));
    expect(groups['backlog'], isEmpty);
  });

  test('outbox title parses requestJson and falls back to remote path', () {
    expect(
      automationOutboxTitle({
        'id': 'o1',
        'requestJson': jsonEncode({'title': ' 我的任务 '}),
        'remoteProjectPath': '/tmp/remote',
      }),
      '我的任务',
    );
    expect(
      automationOutboxTitle({'id': 'o2', 'remoteProjectPath': '/tmp/x'}),
      '/tmp/x',
    );
  });

  test('creates task through task-views/create with action and idempotency key', () async {
    final http = LanHttpClient();
    addTearDown(http.close);
    final client = AutomationClient(http, baseUrl);
    final created = await client.createTask(
      projectId: 'p1',
      title: 'cover browser',
      goal: 'ship live preview',
      acceptanceCriteria: 'preview opens',
      clientRequestId: 'req-1',
      createAction: 'start',
    );
    expect(automationTaskOfView(created)?['id'], 't2');
    final body = captured['/api/orchestrator/task-views/create']!;
    expect(body['title'], 'cover browser');
    expect(body['createAction'], 'start');
    expect(body['priority'], 0);
    expect(body['clientRequestId'], 'req-1');
  });

  test('legacy flat task list and detail routes stay on tasks/list', () async {
    final http = LanHttpClient();
    addTearDown(http.close);
    final client = AutomationClient(http, baseUrl);
    final tasks = await client.listTasks('p1');
    expect(tasks.single['id'], 't1');
    expect(captured['/api/orchestrator/tasks/list']?['projectId'], 'p1');
    final detail = await client.taskDetail('p1', 't1');
    expect(detail['id'], 't1');
  });

  test('completePrompt posts prompt and returns structured fields', () async {
    final http = LanHttpClient();
    addTearDown(http.close);
    final client = AutomationClient(http, baseUrl);
    final completed = await client.completePrompt(
      'p1',
      '修一下登录',
      workingDirectory: '/repo/demo',
    );
    expect(completed['title'], 'AI 标题');
    expect(completed['goal'], 'AI 目标');
    expect(completed['acceptanceCriteria'], 'AI 验收');
    final body = captured['/api/orchestrator/tasks/complete-prompt']!;
    expect(body['projectId'], 'p1');
    expect(body['prompt'], '修一下登录');
    expect(body['workingDirectory'], '/repo/demo');
  });

  test('lists evidence timeline for a task', () async {
    final http = LanHttpClient();
    addTearDown(http.close);
    final client = AutomationClient(http, baseUrl);
    final evidence = await client.listEvidence('p1', 't1');
    expect(evidence.single['kind'], 'verificationOutput');
    expect(evidence.single['content'], 'all green');
    final body = captured['/api/orchestrator/tasks/evidence']!;
    expect(body['projectId'], 'p1');
    expect(body['taskId'], 't1');
  });

  test('retry and discard hit dedicated outbox routes', () async {
    final http = LanHttpClient();
    addTearDown(http.close);
    final client = AutomationClient(http, baseUrl);
    final retried = await client.retryOutbox('p1', 'o1');
    expect(retried['status'], 'pending');
    expect(captured['/api/orchestrator/outbox/retry']!['outboxId'], 'o1');
    final discarded = await client.discardOutbox('p1', 'o1');
    expect(discarded['status'], 'discarded');
    expect(captured['/api/orchestrator/outbox/discard']!['projectId'], 'p1');
  });

  test('listOutbox derives pendingRemote items from task views', () async {
    final http = LanHttpClient();
    addTearDown(http.close);
    final client = AutomationClient(http, baseUrl);
    final outbox = await client.listOutbox('p1');
    expect(outbox.single['id'], 'o1');
    final snapshot = await client.runtimeSnapshot('p1');
    expect(captured['/api/mobile/orchestrator/runtime-snapshot']?['projectId'], 'p1');
    expect(snapshot['remoteStatus'], 'local');
    final experiments = await client.listExperiments('p1');
    expect(experiments.single['id'], 'e1');
  });
}
