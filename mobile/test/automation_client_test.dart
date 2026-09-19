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
      } else if (request.uri.path.endsWith('/experiments/approve-winner')) {
        request.response.write(
          jsonEncode({
            'id': payload['experimentId'],
            'status': 'completed',
            'winnerTaskId': payload['winnerTaskId'],
          }),
        );
      } else if (request.uri.path.endsWith('/experiments/cancel')) {
        request.response.write(
          jsonEncode({'id': payload['experimentId'], 'status': 'cancelled'}),
        );
      } else if (request.uri.path.endsWith('/task-views/create-block')) {
        request.response.write(
          jsonEncode({
            'block': {
              'id': payload['clientRequestId'],
              'projectId': payload['projectId'],
              'title': payload['title'],
            },
            'tasks': [
              {
                'origin': 'local',
                'task': {
                  'id': 'blk-a',
                  'title': (payload['members'] as List).first['title'],
                  'goal': 'g',
                  'workflowState': 'backlog',
                  'blockId': 'blk1',
                  'blockIndex': 0,
                },
              },
              {
                'origin': 'local',
                'task': {
                  'id': 'blk-b',
                  'title': (payload['members'] as List).last['title'],
                  'goal': 'g',
                  'workflowState': 'backlog',
                  'blockId': 'blk1',
                  'blockIndex': 1,
                },
              },
            ],
          }),
        );
      } else if (request.uri.path.endsWith('/task-views/append-block-member')) {
        request.response.write(
          jsonEncode({
            'origin': 'local',
            'task': {
              'id': 'blk-c',
              'title': payload['title'],
              'goal': payload['goal'],
              'workflowState': 'backlog',
              'blockId': payload['blockId'],
              'blockIndex': 2,
            },
          }),
        );
      } else if (request.uri.path
          .endsWith('/task-views/reorder-block-members')) {
        final ordered = (payload['orderedTaskIds'] as List)
            .asMap()
            .entries
            .map((entry) => {
                  'origin': 'local',
                  'task': {
                    'id': entry.value,
                    'title': 'step',
                    'goal': 'g',
                    'workflowState': 'backlog',
                    'blockId': payload['blockId'],
                    'blockIndex': entry.key,
                  },
                })
            .toList();
        // reorder-block-members 响应为裸数组（对齐 web arrayDecoder）。
        request.response.write(jsonEncode(ordered));
      } else if (request.uri.path.endsWith('/mobile/devices')) {
        request.response.write(
          jsonEncode({
            'devices': [
              {
                'id': 'd2',
                'name': 'Office PC',
                'protoVersion': 1,
                'capabilities': ['orchestrator.task-blocks.v1'],
              },
              {
                'id': 'd3',
                'name': 'Old PC',
                'protoVersion': 0,
                'capabilities': <String>[],
              },
            ],
          }),
        );
      } else if (request.uri.path.endsWith('/projects/list')) {
        request.response.write(
          jsonEncode({
            'projects': [
              {'id': 'p1', 'deviceId': 'd2'},
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

  test('createBlock posts block payload and upserts member views', () async {
    final http = LanHttpClient();
    addTearDown(http.close);
    final client = AutomationClient(http, baseUrl);
    final created = await client.createBlock(
      projectId: 'p1',
      title: 'Login block',
      members: const [
        {'title': 'step one', 'goal': 'g1', 'acceptanceCriteria': 'a1'},
        {'title': 'step two', 'goal': 'g2', 'acceptanceCriteria': 'a2'},
      ],
      createAction: 'start',
      clientRequestId: 'req-block-1',
    );
    final body = captured['/api/orchestrator/task-views/create-block']!;
    expect(body['title'], 'Login block');
    expect(body['createAction'], 'start');
    expect(body['clientRequestId'], 'req-block-1');
    expect((body['members'] as List).length, 2);
    expect((body['members'] as List).first['title'], 'step one');
    // 裸任务 DTO 回落为 local view 后可全部 upsert。
    final views = automationUpsertBlockCreated([], created);
    expect(views, hasLength(2));
    expect(automationTaskOfView(views.first)?['blockId'], 'blk1');
  });

  test('appendBlockMember posts three fields and returns member view', () async {
    final http = LanHttpClient();
    addTearDown(http.close);
    final client = AutomationClient(http, baseUrl);
    final created = await client.appendBlockMember(
      projectId: 'p1',
      blockId: 'blk1',
      title: 'step three',
      goal: 'g3',
      acceptanceCriteria: 'a3',
      clientRequestId: 'req-append-1',
    );
    final body = captured['/api/orchestrator/task-views/append-block-member']!;
    expect(body['blockId'], 'blk1');
    expect(body['title'], 'step three');
    expect(body['clientRequestId'], 'req-append-1');
    expect(automationTaskOfView(created)?['id'], 'blk-c');
  });

  test('reorderBlockMembers posts full permutation and returns bare list', () async {
    final http = LanHttpClient();
    addTearDown(http.close);
    final client = AutomationClient(http, baseUrl);
    final updated = await client.reorderBlockMembers(
      projectId: 'p1',
      blockId: 'blk1',
      orderedTaskIds: const ['blk-b', 'blk-a'],
      clientRequestId: 'req-reorder-1',
    );
    final body = captured['/api/orchestrator/task-views/reorder-block-members']!;
    expect(body['orderedTaskIds'], ['blk-b', 'blk-a']);
    expect(updated, hasLength(2));
    expect(automationTaskOfView(updated.first)?['id'], 'blk-b');
    expect(automationTaskOfView(updated.first)?['blockIndex'], 0);
  });

  test('experiment approve-winner and cancel hit dedicated routes', () async {
    final http = LanHttpClient();
    addTearDown(http.close);
    final client = AutomationClient(http, baseUrl);
    final approved = await client.approveExperimentWinner('e1', 'cand-2');
    expect(
      captured['/api/orchestrator/experiments/approve-winner']!,
      containsPair('experimentId', 'e1'),
    );
    expect(
      captured['/api/orchestrator/experiments/approve-winner']!['winnerTaskId'],
      'cand-2',
    );
    expect(approved['status'], 'completed');
    final cancelled = await client.cancelExperiment('e1');
    expect(
      captured['/api/orchestrator/experiments/cancel']!['experimentId'],
      'e1',
    );
    expect(cancelled['status'], 'cancelled');
  });

  test('owner peer devices expose task-block capability', () async {
    final http = LanHttpClient();
    addTearDown(http.close);
    final client = AutomationClient(http, baseUrl);
    final deviceId = await client.projectOwnerDeviceId('p1');
    expect(deviceId, 'd2');
    expect(captured['/api/mobile/workbench/projects/list'], isNotNull);
    final devices = await client.listDevices();
    final owner = devices.firstWhere((device) => device['id'] == deviceId);
    expect(automationPeerSupportsTaskBlocks(owner), isTrue);
    expect(
      automationPeerSupportsTaskBlocks(
        devices.firstWhere((device) => device['id'] == 'd3'),
      ),
      isFalse,
    );
  });

  test('board grouping aggregates blocks into head lane with sorted members', () {
    Map<String, dynamic> member(
      String id, {
      int blockIndex = 0,
      String workflowState = 'backlog',
      String runState = 'idle',
      String createdAt = '2026-09-01T00:00:00Z',
    }) =>
        {
          'origin': 'local',
          'task': {
            'id': id,
            'title': id,
            'goal': 'g',
            'workflowState': workflowState,
            'runState': runState,
            'blockId': 'blk1',
            'blockTitle': 'Login block',
            'blockIndex': blockIndex,
            'createdAt': createdAt,
          },
        };
    final groups = automationGroupBoardItems([
      member('m2', blockIndex: 1),
      member('m1', blockIndex: 0),
      {
        'origin': 'local',
        'task': {
          'id': 'solo',
          'title': 'solo',
          'workflowState': 'inProgress',
        },
      },
    ]);
    // 块整块落在 head 泳道（backlog），成员按 blockIndex 排序。
    expect(groups['backlog'], hasLength(1));
    final block = groups['backlog']!.single;
    expect(block.isBlock, isTrue);
    expect(block.blockId, 'blk1');
    expect(block.title, 'Login block');
    expect(block.members.map((m) => m.id).toList(), ['m1', 'm2']);
    // 无 blockId 任务按自身泳道独立成卡。
    expect(groups['inProgress']!.single.isBlock, isFalse);
    // 全部成员终态时块落在 done。
    final doneGroups = automationGroupBoardItems([
      member('m1', workflowState: 'done'),
      member('m2', blockIndex: 1, workflowState: 'canceled'),
    ]);
    expect(doneGroups['done']!.single.isBlock, isTrue);
  });

  test('block append/reorder gates follow web board rules', () {
    AutomationRenderableTask member({
      String workflowState = 'backlog',
      String runState = 'idle',
    }) {
      final view = {
        'origin': 'local',
        'task': {
          'id': workflowState + runState,
          'title': 't',
          'workflowState': workflowState,
          'runState': runState,
          'blockId': 'blk1',
          'blockIndex': 0,
        },
      };
      return AutomationRenderableTask(
        origin: 'local',
        task: view['task'] as Map<String, dynamic>,
        view: view,
      );
    }

    final idleBacklog = [member(), member()];
    expect(automationCanReorderBlock(idleBacklog), isTrue);
    expect(automationCanAppendToBlock(idleBacklog), isTrue);
    // 运行中的成员禁止重排；进入复核的成员禁止追加。
    final running = [member(), member(runState: 'running')];
    expect(automationCanReorderBlock(running), isFalse);
    final reviewing = [member(), member(workflowState: 'humanReview')];
    expect(automationCanAppendToBlock(reviewing), isFalse);
    // 已达上限的块不再追加。
    final full = List.generate(
      kAutomationBlockMaxMembers,
      (_) => member(),
    );
    expect(automationCanAppendToBlock(full), isFalse);
  });

  test('task-block capability gate mirrors web semantics', () {
    // 本机项目始终可建块；无 kind fail-closed。
    expect(
      automationCanCreateTaskBlock(projectKind: 'local', peer: null),
      isTrue,
    );
    expect(automationCanCreateTaskBlock(projectKind: null), isFalse);
    // remote 缺 peer / v0 / 缺 token 都拒绝。
    expect(automationCanCreateTaskBlock(projectKind: 'remote'), isFalse);
    expect(
      automationCanCreateTaskBlock(projectKind: 'remote', peer: const {
        'protoVersion': 1,
      }),
      isFalse,
    );
    expect(
      automationCanCreateTaskBlock(projectKind: 'remote', peer: const {
        'protocol_version': 1,
        'capabilities': ['other.v1'],
      }),
      isFalse,
    );
    expect(
      automationCanCreateTaskBlock(projectKind: 'remote', peer: const {
        'protocol_version': 1,
        'capabilities': ['orchestrator.task-blocks.v1'],
      }),
      isTrue,
    );
  });

  test('view upsert helpers keep stable keys for task and pending views', () {
    final taskView = {
      'origin': 'local',
      'task': {'id': 't1', 'title': 'old'},
    };
    final pendingView = {
      'origin': 'pendingRemote',
      'item': {'id': 'o1', 'status': 'pending'},
    };
    // 相同 key 替换，不追加。
    final replaced = automationUpsertView([taskView, pendingView], {
      'origin': 'local',
      'task': {'id': 't1', 'title': 'new'},
    });
    expect(replaced, hasLength(2));
    expect(automationTaskOfView(replaced[0])?['title'], 'new');
    // 新 key 插入头部。
    final inserted = automationUpsertView([taskView], pendingView);
    expect(inserted.first['origin'], 'pendingRemote');
  });

  test('experiment helpers expose needsDecision and recommended winner', () {
    final needsDecision = {
      'id': 'e1',
      'status': 'needsDecision',
      'winnerTaskId': null,
      'candidates': [
        {
          'taskId': 'cand-1',
          'ordinal': 1,
          'outcome': 'candidateReady',
        },
        {
          'taskId': 'cand-2',
          'ordinal': 2,
          'outcome': 'pending',
        },
      ],
    };
    expect(automationExperimentNeedsDecision(needsDecision), isTrue);
    expect(automationExperimentRecommendedTaskId(needsDecision), 'cand-1');
    // winnerTaskId 优先于 candidate 顺序。
    expect(
      automationExperimentRecommendedTaskId({
        ...needsDecision,
        'status': 'winnerReady',
        'winnerTaskId': 'cand-2',
      }),
      'cand-2',
    );
    // 非决策态不触发动作。
    expect(
      automationExperimentNeedsDecision({
        'id': 'e2',
        'status': 'running',
      }),
      isFalse,
    );
  });
}
