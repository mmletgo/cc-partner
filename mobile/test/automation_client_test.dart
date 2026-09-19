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
      if (request.uri.path.endsWith('/tasks/list')) {
        request.response.write(
          jsonEncode({
            'tasks': [
              {'id': 't1', 'title': 'ship app', 'projectId': payload['projectId']},
            ],
          }),
        );
      } else if (request.uri.path.endsWith('/tasks/create')) {
        request.response.write(
          jsonEncode({
            'id': 't2',
            'title': payload['title'],
            'goal': payload['goal'],
          }),
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
      } else if (request.uri.path.endsWith('/task-views/list')) {
        request.response.write(
          jsonEncode({
            'views': [
              {
                'id': 'o1',
                'kind': 'pendingRemote',
                'status': 'failed',
                'title': 'queued remote',
              },
            ],
          }),
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

  test('lists and creates orchestrator tasks on existing mobile routes', () async {
    final http = LanHttpClient();
    addTearDown(http.close);
    final client = AutomationClient(http, baseUrl);
    final tasks = await client.listTasks('p1');
    expect(tasks.single['id'], 't1');
    expect(captured['/api/orchestrator/tasks/list']?['projectId'], 'p1');
    final created = await client.createTask(
      projectId: 'p1',
      title: 'cover browser',
      goal: 'ship live preview',
      acceptanceCriteria: 'preview opens',
      clientRequestId: 'req-1',
    );
    expect(created['id'], 't2');
    expect(captured['/api/orchestrator/tasks/create']?['title'], 'cover browser');
    expect(captured['/api/orchestrator/tasks/create']?['clientRequestId'], 'req-1');
    final snapshot = await client.runtimeSnapshot('p1');
    expect(captured['/api/mobile/orchestrator/runtime-snapshot']?['projectId'], 'p1');
    expect(snapshot['remoteStatus'], 'local');
    final experiments = await client.listExperiments('p1');
    expect(experiments.single['id'], 'e1');
    final outbox = await client.listOutbox('p1');
    expect(outbox, isNotEmpty);
  });
}
