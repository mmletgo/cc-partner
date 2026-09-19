import 'dart:convert';
import 'dart:io';

import 'package:cc_partner_mobile/core/lan_http.dart';
import 'package:cc_partner_mobile/projects/client.dart';
import 'package:test/test.dart';

void main() {
  late HttpServer server;
  late String baseUrl;

  setUp(() async {
    server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    baseUrl = 'http://127.0.0.1:${server.port}';
    server.listen((request) async {
      final body = await utf8.decodeStream(request);
      Map<String, dynamic> payload = const {};
      if (body.isNotEmpty) {
        payload = jsonDecode(body) as Map<String, dynamic>;
      }
      request.response.headers.contentType = ContentType.json;
      if (request.uri.path.endsWith('/projects/list')) {
        request.response.write(
          jsonEncode({
            'projects': [
              {'id': 'p1', 'name': 'demo', 'kind': 'local'},
            ],
          }),
        );
      } else if (request.uri.path.endsWith('/projects/open')) {
        request.response.write(
          jsonEncode({
            'project': {
              'id': 'p-new',
              'name': payload['path'],
              'kind': 'local',
            },
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
}
