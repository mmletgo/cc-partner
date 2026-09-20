import 'dart:convert';
import 'dart:io';

import 'package:cc_partner_mobile/core/lan_http.dart';
import 'package:cc_partner_mobile/attention/client.dart';
import 'package:cc_partner_mobile/attention/filter.dart';
import 'package:test/test.dart';

/// 构造一条与后端 AttentionItemDto（camelCase）一致的快照条目。
Map<String, dynamic> snapshotItem({
  required String id,
  String sourceKind = 'agentNeedsInput',
  String targetKind = 'agentSession',
  String? readAt,
  String? freshness = 'live',
  String? cachedAt = '2026-09-19T07:30:00Z',
  String? taskId,
  String? outboxId,
}) =>
    {
      'id': id,
      'category': 'blocked',
      'sourceKind': sourceKind,
      'title': '标题 $id',
      'summary': '摘要 $id',
      'updatedAt': '2026-09-19T08:00:00Z',
      'freshness': freshness,
      'cachedAt': cachedAt,
      'project': {'id': 'p1', 'name': 'demo', 'kind': 'local'},
      'device': {'id': 'pc-a', 'name': 'Hans Mac'},
      'target': {
        'kind': targetKind,
        'projectId': 'p1',
        'terminalSessionId': 'tmux-1',
        'worktreeId': 'wt-1',
        if (taskId != null) 'taskId': taskId,
        if (outboxId != null) 'outboxId': outboxId,
      },
      if (readAt != null) 'readAt': readAt,
    };

Map<String, dynamic> snapshot(List<Map<String, dynamic>> items) => {
      'generatedAt': '2026-09-19T08:00:00Z',
      'counts': {'total': items.length},
      'items': items,
      'myDeviceId': 'pc-a',
    };

void main() {
  late HttpServer server;
  late String baseUrl;
  final captured = <String, Map<String, dynamic>>{};

  /// /api/health 响应体；各用例可覆盖以模拟旧后端或缺能力（null 还原默认）。
  Map<String, dynamic>? healthOverride;
  final defaultHealth = <String, dynamic>{
    'protocol_version': 1,
    'capabilities': ['attention.v1', 'attention.v2'],
  };

  setUp(() async {
    captured.clear();
    healthOverride = null;
    server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    baseUrl = 'http://127.0.0.1:${server.port}';
    server.listen((request) async {
      final body = await utf8.decodeStream(request);
      Map<String, dynamic> payload = const {};
      if (body.isNotEmpty) {
        payload = jsonDecode(body) as Map<String, dynamic>;
      }
      captured['${request.method} ${request.uri.path}'] = payload;
      request.response.headers.contentType = ContentType.json;
      if (request.uri.path == '/api/health') {
        request.response.write(jsonEncode(healthOverride ?? defaultHealth));
      } else if (request.uri.path == '/api/mobile/attention/v2' && !captured.containsKey('fail-v2')) {
        request.response.write(jsonEncode(snapshot([
          snapshotItem(id: 'unread-1'),
          snapshotItem(id: 'read-1', readAt: '2026-09-19T09:00:00Z'),
          snapshotItem(id: 'hidden-1', sourceKind: 'workbenchDependency', targetKind: 'settings'),
        ])));
      } else if (request.uri.path == '/api/mobile/attention/v2') {
        request.response.statusCode = 500;
        request.response.write('{}');
      } else if (request.uri.path == '/api/mobile/attention') {
        request.response.write(jsonEncode(snapshot([snapshotItem(id: 'v1-1')])));
      } else if (request.uri.path == '/api/mobile/attention/mark-read') {
        request.response.write(
          jsonEncode(snapshot([snapshotItem(id: 'unread-1', readAt: '2026-09-19T10:00:00Z')])),
        );
      } else if (request.uri.path == '/api/mobile/attention/mark-unread') {
        request.response.write(jsonEncode(snapshot([snapshotItem(id: 'read-1')])));
      } else if (request.uri.path == '/api/mobile/attention/mark-all-read') {
        request.response.write(
          jsonEncode(snapshot([snapshotItem(id: 'unread-1', readAt: '2026-09-19T10:00:00Z')])),
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

  test('listVisible reads v2 snapshot, parses read state and hides tmux items', () async {
    final http = LanHttpClient();
    addTearDown(http.close);
    final client = AttentionClient(http, baseUrl);
    final items = await client.listVisible();
    expect(items, hasLength(2));
    expect(items[0].id, 'unread-1');
    expect(items[0].isUnread, isTrue);
    expect(items[0].projectName, 'demo');
    expect(items[0].deviceName, 'Hans Mac');
    expect(items[0].updatedAt, '2026-09-19T08:00:00Z');
    // freshness/cachedAt 解析（徽章与「最后同步于」展示依据）。
    expect(items[0].freshness, 'live');
    expect(items[0].cachedAt, '2026-09-19T07:30:00Z');
    expect(items[1].id, 'read-1');
    expect(items[1].isUnread, isFalse);
  });

  test('attention item parses automation focus ids from the semantic target', () {
    final taskItem = AttentionItem.fromJson({
      'id': 'task-1',
      'sourceKind': 'orchestratorHumanReview',
      'category': 'decision',
      'target': {'kind': 'orchestratorTask', 'projectId': 'p1', 'taskId': 'task-9'},
    });
    expect(taskItem.targetKind, 'orchestratorTask');
    expect(taskItem.taskId, 'task-9');
    final nav = navigateAttention(taskItem);
    expect(nav.panel, 'automation');
    expect(nav.taskId, 'task-9');

    final outboxItem = AttentionItem.fromJson({
      'id': 'outbox-1',
      'sourceKind': 'remoteOutboxFailed',
      'category': 'blocked',
      'target': {'kind': 'remoteOutbox', 'projectId': 'p1', 'outboxId': 'outbox-7'},
    });
    expect(outboxItem.outboxId, 'outbox-7');
    final outboxNav = navigateAttention(outboxItem);
    expect(outboxNav.panel, 'automation');
    expect(outboxNav.outboxId, 'outbox-7');

    // experiment 只导航不聚焦（与 web 一致）。
    final experimentNav = navigateAttention(AttentionItem.fromJson({
      'id': 'exp-1',
      'sourceKind': 'experimentNeedsDecision',
      'category': 'decision',
      'target': {'kind': 'experiment', 'projectId': 'p1', 'experimentId': 'exp-7'},
    }));
    expect(experimentNav.panel, 'automation');
    expect(experimentNav.taskId, isNull);
    expect(experimentNav.outboxId, isNull);
  });

  test('groupAttentionItems buckets by category and trails unknown categories', () {
    final groups = groupAttentionItems([
      _rawItem('b1', 'blocked'),
      _rawItem('d1', 'decision'),
      _rawItem('e1', 'environment'),
      _rawItem('x1', 'weird'),
      _rawItem('x2', null),
    ]);
    expect(groups.map((g) => g.category).toList(), [
      'decision',
      'blocked',
      'environment',
      'other',
    ]);
    expect(groups[0].items.single.id, 'd1');
    expect(groups[3].items.map((i) => i.id), ['x1', 'x2']);
    expect(attentionGroupLabel('other'), '其他');
    expect(attentionGroupLabel('blocked'), '运行受阻');
  });

  test('listVisible falls back to v1 when v2 fails', () async {
    captured['fail-v2'] = const {'trigger': true};
    final http = LanHttpClient();
    addTearDown(http.close);
    final client = AttentionClient(http, baseUrl);
    final items = await client.listVisible();
    expect(items, hasLength(1));
    expect(items.single.id, 'v1-1');
  });

  test('listVisible probes /api/health before loading the snapshot', () async {
    final http = LanHttpClient();
    addTearDown(http.close);
    final client = AttentionClient(http, baseUrl);
    await client.listVisible();
    // 能力探测先行（对齐 web assertAttentionCapability）。
    expect(captured.containsKey('GET /api/health'), isTrue);
    // 默认 health 带 v2：走 v2，不打 v1。
    expect(captured.containsKey('GET /api/mobile/attention/v2'), isTrue);
    expect(captured.containsKey('GET /api/mobile/attention'), isFalse);
  });

  test('health with attention.v1 only skips v2 and loads v1 directly', () async {
    healthOverride = {
      'protocol_version': 1,
      'capabilities': ['attention.v1'],
    };
    final http = LanHttpClient();
    addTearDown(http.close);
    final client = AttentionClient(http, baseUrl);
    final items = await client.listVisible();
    expect(items.single.id, 'v1-1');
    expect(captured.containsKey('GET /api/mobile/attention/v2'), isFalse);
    expect(captured.containsKey('GET /api/mobile/attention'), isTrue);
  });

  test('health without attention.v1 throws AttentionUnsupportedError', () async {
    healthOverride = {
      'protocol_version': 1,
      'capabilities': ['provider-manager.v1'],
    };
    final http = LanHttpClient();
    addTearDown(http.close);
    final client = AttentionClient(http, baseUrl);
    expect(
      client.listVisible(),
      throwsA(isA<AttentionUnsupportedError>()),
    );
  });

  test('legacy health without protocol fields counts as unsupported', () async {
    // 旧后端缺 protocol_version/capabilities：安全回落为不支持（fail-closed）。
    healthOverride = <String, dynamic>{};
    final http = LanHttpClient();
    addTearDown(http.close);
    final client = AttentionClient(http, baseUrl);
    expect(client.listVisible(), throwsA(isA<AttentionUnsupportedError>()));
  });

  test('AttentionHealthInfo parses tolerantly and matches web gates', () {
    // v1/v2 判定与 web supportsAttentionV1/V2 同口径：version>=1 且精确包含。
    const info = AttentionHealthInfo(protocolVersion: 1, capabilities: [
      attentionCapabilityV1,
      attentionCapabilityV2,
    ]);
    expect(info.supportsV1, isTrue);
    expect(info.supportsV2, isTrue);
    expect(
      const AttentionHealthInfo(protocolVersion: 1, capabilities: ['attention.v1'])
          .supportsV2,
      isFalse,
    );
    expect(
      const AttentionHealthInfo(protocolVersion: 0, capabilities: [
        attentionCapabilityV1,
      ]).supportsV1,
      isFalse,
    );
    // 宽容解析：缺字段按不支持处理。
    final legacy = AttentionHealthInfo.fromJson(<String, dynamic>{});
    expect(legacy.supportsV1, isFalse);
    expect(legacy.supportsV2, isFalse);
    final nonListCaps =
        AttentionHealthInfo.fromJson({'protocol_version': 'x', 'capabilities': 'nope'});
    expect(nonListCaps.protocolVersion, 0);
    expect(nonListCaps.capabilities, isEmpty);
  });

  test('markRead posts itemIds and returns the updated snapshot items', () async {
    final http = LanHttpClient();
    addTearDown(http.close);
    final client = AttentionClient(http, baseUrl);
    final items = await client.markRead(['unread-1']);
    expect(captured['POST /api/mobile/attention/mark-read'], {
      'itemIds': ['unread-1'],
    });
    expect(items.single.id, 'unread-1');
    expect(items.single.isUnread, isFalse);
  });

  test('markUnread posts itemIds to the unread route', () async {
    final http = LanHttpClient();
    addTearDown(http.close);
    final client = AttentionClient(http, baseUrl);
    final items = await client.markUnread(['read-1']);
    expect(captured['POST /api/mobile/attention/mark-unread'], {
      'itemIds': ['read-1'],
    });
    expect(items.single.id, 'read-1');
    expect(items.single.isUnread, isTrue);
  });

  test('markAllRead posts an empty body to the all-read route', () async {
    final http = LanHttpClient();
    addTearDown(http.close);
    final client = AttentionClient(http, baseUrl);
    final items = await client.markAllRead();
    expect(captured['POST /api/mobile/attention/mark-all-read'], const {});
    expect(items.single.id, 'unread-1');
    expect(items.single.isUnread, isFalse);
  });
}

AttentionItem _rawItem(String id, String? category) => AttentionItem(
      id: id,
      sourceKind: 'agentNeedsInput',
      targetKind: 'agentSession',
      category: category,
    );
