import 'dart:convert';
import 'dart:io';

import 'package:cc_partner_mobile/browser/client.dart';
import 'package:cc_partner_mobile/core/lan_http.dart';
import 'package:cc_partner_mobile/files/html_preview.dart';
import 'package:test/test.dart';

void main() {
  test('browser live preview is a separate JS-on channel from HTML file preview', () {
    expect(HtmlPreviewPolicy.scriptsEnabled, isFalse);
    expect(BrowserPreviewPolicy.scriptsEnabled, isTrue);
    expect(BrowserPreviewPolicy.proxyPath('abc'), '/api/mobile/workbench/browser/proxy/abc/');
  });

  test('creates a live preview on the existing mobile browser route', () async {
    final server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    addTearDown(() => server.close(force: true));
    Map<String, dynamic>? body;
    String? path;
    server.listen((request) async {
      path = request.uri.path;
      final raw = await utf8.decodeStream(request);
      body = jsonDecode(raw) as Map<String, dynamic>;
      request.response.headers.contentType = ContentType.json;
      request.response.write(
        jsonEncode({
          'previewId': 'prev-1',
          'mobileProxyPath': '/api/mobile/workbench/browser/proxy/prev-1/',
          'targetUrl': body!['targetUrl'],
        }),
      );
      await request.response.close();
    });
    final http = LanHttpClient();
    addTearDown(http.close);
    final client = BrowserClient(http, 'http://127.0.0.1:${server.port}');
    final preview = await client.createPreview(
      projectId: 'p1',
      worktreeId: 'wt1',
      targetUrl: 'http://127.0.0.1:5173',
    );
    expect(path, '/api/mobile/workbench/browser/preview');
    expect(body?['projectId'], 'p1');
    expect(preview.previewId, 'prev-1');
    expect(preview.scriptsEnabled, isTrue);
  });

  test('discover parses dev server candidates and passes project ids', () async {
    final server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    addTearDown(() => server.close(force: true));
    String? path;
    Map<String, dynamic>? body;
    server.listen((request) async {
      path = request.uri.path;
      final raw = await utf8.decodeStream(request);
      body = jsonDecode(raw) as Map<String, dynamic>;
      request.response.headers.contentType = ContentType.json;
      request.response.write(
        jsonEncode({
          'projectId': 'p1',
          'worktreeId': null,
          'targets': [
            {
              'id': 't-1',
              'url': 'http://127.0.0.1:5173',
              'displayUrl': '127.0.0.1:5173',
              'source': 'projectConfig',
              'reachable': true,
            },
            {'id': 't-2', 'url': 'http://127.0.0.1:3000'},
          ],
          'selectedTargetId': 't-1',
        }),
      );
      await request.response.close();
    });
    final http = LanHttpClient();
    addTearDown(http.close);
    final client = BrowserClient(http, 'http://127.0.0.1:${server.port}');
    final discovery = await client.discover(projectId: 'p1');
    expect(path, '/api/mobile/workbench/browser/discover');
    expect(body?['projectId'], 'p1');
    expect(body?['worktreeId'], isNull);
    expect(discovery.targets, hasLength(2));
    expect(discovery.targets.first.url, 'http://127.0.0.1:5173');
    expect(discovery.targets.first.label, '127.0.0.1:5173');
    expect(discovery.targets.first.reachable, isTrue);
    expect(discovery.targets.last.label, 'http://127.0.0.1:3000');
    expect(discovery.selectedTarget?.id, 't-1');
  });
}
