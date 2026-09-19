import 'dart:convert';
import 'dart:io';

import 'package:cc_partner_mobile/core/lan_http.dart';
import 'package:cc_partner_mobile/projects/client.dart';
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
      final body = await utf8.decodeStream(request);
      Map<String, dynamic> payload = const {};
      if (body.isNotEmpty) {
        payload = jsonDecode(body) as Map<String, dynamic>;
      }
      captured[request.uri.path] = payload;
      request.response.headers.contentType = ContentType.json;
      if (request.uri.path.endsWith('/projects/list')) {
        expect(request.method, 'GET');
        request.response.write(
          jsonEncode([
            {'id': 'p1', 'name': 'demo', 'kind': 'local'},
          ]),
        );
      } else if (request.uri.path.endsWith('/projects/open')) {
        request.response.write(
          jsonEncode({
            'project': {
              'id': 'p-new',
              'name': payload['path'],
              'kind': 'local',
              'path': payload['path'],
            },
          }),
        );
      } else if (request.uri.path.endsWith('/remote/open')) {
        request.response.write(
          jsonEncode({
            'id': 'p-lan',
            'name': payload['path'],
            'kind': 'remote',
            'path': payload['path'],
          }),
        );
      } else if (request.uri.path.endsWith('/fs/create-dir') ||
          request.uri.path.endsWith('/remote/create-dir')) {
        request.response.write(
          jsonEncode({
            'path': '${payload['parentPath']}/${payload['name']}',
            'kind': 'directory',
          }),
        );
      } else if (request.uri.path.endsWith('/fs/roots')) {
        request.response.write(
          jsonEncode([
            {'path': '/Users/demo', 'label': 'Home'},
          ]),
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

  test('lists recent projects on the current PC', () async {
    final http = LanHttpClient();
    addTearDown(http.close);
    final client = ProjectsClient(http, baseUrl);
    final list = await client.listRecent();
    expect(list, hasLength(1));
    expect(list.single.id, 'p1');
  });

  test('opens a local directory and can create one nested folder', () async {
    final http = LanHttpClient();
    addTearDown(http.close);
    final client = ProjectsClient(http, baseUrl);
    final created = await client.createDir(parentPath: '/Users/demo', name: 'new-app');
    expect(created['path'], '/Users/demo/new-app');
    expect(captured['/api/mobile/workbench/fs/create-dir'], {
      'parentPath': '/Users/demo',
      'name': 'new-app',
    });
    final opened = await client.open(path: '/Users/demo/new-app');
    expect(opened.id, 'p-new');
    expect(opened.path, '/Users/demo/new-app');
  });

  test('opens a LAN directory via host remote proxy and create-dir', () async {
    final http = LanHttpClient();
    addTearDown(http.close);
    final client = ProjectsClient(http, baseUrl);
    final created = await client.createRemoteDir(
      deviceId: 'pc-b',
      parentPath: '/srv',
      name: 'lan-app',
    );
    expect(created['path'], '/srv/lan-app');
    expect(captured['/api/mobile/workbench/remote/create-dir'], {
      'deviceId': 'pc-b',
      'parentPath': '/srv',
      'name': 'lan-app',
    });
    final opened = await client.openRemote(deviceId: 'pc-b', path: '/srv/lan-app');
    expect(opened.kind, 'remote');
    expect(captured['/api/mobile/workbench/remote/open'], {
      'deviceId': 'pc-b',
      'path': '/srv/lan-app',
    });
  });
}
