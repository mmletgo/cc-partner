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
      } else if (request.uri.path.endsWith('/fs/info') ||
          request.uri.path.endsWith('/remote/info')) {
        final path = payload['path'] as String? ?? '';
        request.response.write(
          jsonEncode({
            'name': path.split('/').where((s) => s.isNotEmpty).lastOrNull ?? '/',
            'path': path,
            'kind': 'dir',
            'readable': true,
            'isGitRepo': false,
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

  test('localPathInfo posts to fs/info and parses the readable dir info', () async {
    final http = LanHttpClient();
    addTearDown(http.close);
    final client = ProjectsClient(http, baseUrl);
    final info = await client.localPathInfo('/Users/demo/proj-a');
    expect(captured['/api/mobile/workbench/fs/info'], {'path': '/Users/demo/proj-a'});
    expect(info.path, '/Users/demo/proj-a');
    expect(info.kind, 'dir');
    expect(info.readable, isTrue);
    expect(info.name, 'proj-a');
  });

  test('remotePathInfo posts deviceId+path to remote/info', () async {
    final http = LanHttpClient();
    addTearDown(http.close);
    final client = ProjectsClient(http, baseUrl);
    final info = await client.remotePathInfo(deviceId: 'pc-b', path: '/srv/lan');
    expect(captured['/api/mobile/workbench/remote/info'], {
      'deviceId': 'pc-b',
      'path': '/srv/lan',
    });
    expect(info.path, '/srv/lan');
    expect(info.kind, 'dir');
    expect(info.readable, isTrue);
  });

  test('filterOnlineLanDevices drops offline/self and dedupes relay shadows', () {
    final devices = [
      // 直连在线 peer：保留。
      {'id': 'peer-1', 'name': 'Laptop', 'isSelf': false, 'online': true},
      // 同 id 直连 + 影子并存：只保留直连。
      {
        'id': 'peer-1',
        'name': 'Laptop',
        'isSelf': false,
        'online': true,
        'viaDeviceId': 'nas',
        'viaDeviceName': 'NAS',
      },
      // 离线直连：过滤。
      {'id': 'peer-2', 'name': 'Down', 'isSelf': false, 'online': false},
      // 本机：过滤。
      {'id': 'self-1', 'name': 'Desktop', 'isSelf': true, 'online': true},
      // 仅影子可见的 peer：在线则保留（新 status 字段口径）。
      {
        'id': 'peer-3',
        'name': 'Remote',
        'isSelf': false,
        'status': 'online',
        'viaDeviceId': 'nas',
        'viaDeviceName': 'NAS',
      },
      // 离线影子：同样过滤（对齐 web filterOnlineLanDevices，不同于桌面置灰口径）。
      {
        'id': 'peer-4',
        'name': 'Ghost',
        'isSelf': false,
        'status': 'offline',
        'viaDeviceId': 'nas',
      },
    ];
    final filtered = filterOnlineLanDevices(devices);
    expect(filtered.map(transferDeviceIdOf), ['peer-1', 'peer-3']);
    // 直连排在前、影子在后（与 web dedupeRelayShadowDevices 排序一致）。
    expect(isRelayShadowDevice(filtered.first), isFalse);
    expect(isRelayShadowDevice(filtered.last), isTrue);
  });

  test('transferDeviceStatus tolerates legacy and new payloads', () {
    // 新字段优先。
    expect(
      transferDeviceStatus({'status': 'online', 'online': false}),
      'online',
    );
    // 旧字段推导：online=false → offline。
    expect(transferDeviceStatus({'online': false}), 'offline');
    // 双缺省按在线。
    expect(transferDeviceStatus(<String, dynamic>{}), 'online');
  });

  test('deviceSupportsBrowseMkdir requires the exact create-dir capability', () {
    expect(
      deviceSupportsBrowseMkdir({
        'capabilities': ['transfer.resume.v1', 'workbench.fs.create-dir.v1'],
      }),
      isTrue,
    );
    // 缺能力 / 缺字段 / 未选设备一律 false（fail-closed）。
    expect(deviceSupportsBrowseMkdir({'capabilities': ['transfer.resume.v1']}), isFalse);
    expect(deviceSupportsBrowseMkdir(<String, dynamic>{}), isFalse);
    expect(deviceSupportsBrowseMkdir({'capabilities': 'workbench.fs.create-dir.v1'}), isFalse);
    expect(deviceSupportsBrowseMkdir(null), isFalse);
  });
}

/// 从设备条目取 id（测试内 shortcut，与 transfer.client 的 transferDeviceId 同口径）。
String transferDeviceIdOf(Map<String, dynamic> device) =>
    device['id'] as String? ?? device['deviceId'] as String? ?? '';
