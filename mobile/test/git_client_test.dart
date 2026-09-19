import 'dart:convert';
import 'dart:io';

import 'package:cc_partner_mobile/core/lan_http.dart';
import 'package:cc_partner_mobile/git/client.dart';
import 'package:test/test.dart';

void main() {
  late HttpServer server;
  late String baseUrl;
  String? lastPath;

  setUp(() async {
    server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    baseUrl = 'http://127.0.0.1:${server.port}';
    server.listen((request) async {
      lastPath = request.uri.path;
      await utf8.decodeStream(request);
      request.response.headers.contentType = ContentType.json;
      request.response.write(jsonEncode({'ok': true, 'worktrees': []}));
      await request.response.close();
    });
  });

  tearDown(() async {
    await server.close(force: true);
  });

  test('lists worktrees on the current PC', () async {
    final http = LanHttpClient();
    addTearDown(http.close);
    final git = GitClient(http, baseUrl);
    final body = await git.listWorktrees('p1');
    expect(body['ok'], isTrue);
    expect(lastPath, '/api/mobile/workbench/worktrees/list');
  });
}
