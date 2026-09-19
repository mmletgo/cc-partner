import 'dart:convert';
import 'dart:io';

import 'package:cc_partner_mobile/core/lan_http.dart';
import 'package:cc_partner_mobile/git/client.dart';
import 'package:test/test.dart';

void main() {
  late HttpServer server;
  late String baseUrl;
  String? lastPath;
  Map<String, dynamic> lastBody = const {};

  setUp(() async {
    lastPath = null;
    lastBody = const {};
    server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    baseUrl = 'http://127.0.0.1:${server.port}';
    server.listen((request) async {
      lastPath = request.uri.path;
      final raw = await utf8.decodeStream(request);
      if (raw.isNotEmpty) {
        lastBody = jsonDecode(raw) as Map<String, dynamic>;
      }
      request.response.headers.contentType = ContentType.json;
      if (request.uri.path.endsWith('/worktrees/list')) {
        request.response.write(
          jsonEncode({
            'ok': true,
            'worktrees': [
              {'id': 'wt-main', 'name': 'main', 'branch': 'main', 'isMain': true},
            ],
          }),
        );
      } else if (request.uri.path.endsWith('/worktrees/create')) {
        request.response.write(
          jsonEncode({
            'id': 'wt-feat',
            'name': lastBody['branchName'],
            'branch': lastBody['branchName'],
            'isMain': false,
          }),
        );
      } else {
        request.response.write(jsonEncode({'ok': true, 'kind': 'succeeded'}));
      }
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

  test('creates a worktree and commits with a message', () async {
    final http = LanHttpClient();
    addTearDown(http.close);
    final git = GitClient(http, baseUrl);
    final created = await git.create(projectId: 'p1', branchName: 'feat/app');
    expect(created['id'], 'wt-feat');
    expect(lastPath, '/api/mobile/workbench/worktrees/create');
    await git.commit(
      worktreeId: 'wt-feat',
      clientOperationId: 'op-1',
      message: 'fix: mobile git',
    );
    expect(lastPath, '/api/mobile/workbench/worktrees/commit');
    expect(lastBody['message'], 'fix: mobile git');
    expect(lastBody['clientOperationId'], 'op-1');
  });

  test('repairs a failed git hook on the existing route', () async {
    final http = LanHttpClient();
    addTearDown(http.close);
    final git = GitClient(http, baseUrl);
    await git.repairHookFailure(
      worktreeId: 'wt-feat',
      hookFailure: {'hook': 'commit-msg', 'output': 'blocked'},
    );
    expect(lastPath, '/api/mobile/workbench/worktrees/repair-hook-failure');
    expect(lastBody['worktreeId'], 'wt-feat');
  });
}
