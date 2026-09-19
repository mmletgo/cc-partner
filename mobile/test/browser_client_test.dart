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

  test('startVerification posts previewId and requestId to the create route', () async {
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
          'session': {
            'id': 'run-1',
            'previewId': 'prev-1',
            'state': 'queued',
          },
          'evidence': null,
        }),
      );
      await request.response.close();
    });
    final http = LanHttpClient();
    addTearDown(http.close);
    final client = BrowserClient(http, 'http://127.0.0.1:${server.port}');
    final run = await client.startVerification(
      previewId: 'prev-1',
      requestId: 'req-1',
    );
    expect(path, '/api/workbench/browser-verification/create');
    expect(body, {'previewId': 'prev-1', 'requestId': 'req-1'});
    expect(run.session.id, 'run-1');
    expect(run.session.previewId, 'prev-1');
    expect(run.session.state, 'queued');
    expect(run.evidence, isNull);
    expect(isBrowserVerificationTerminalState('queued'), isFalse);
    expect(isBrowserVerificationTerminalState('running'), isFalse);
    expect(isBrowserVerificationTerminalState('succeeded'), isTrue);
    expect(isBrowserVerificationTerminalState('failed'), isTrue);
    expect(isBrowserVerificationTerminalState('canceled'), isTrue);
  });

  test('getVerification posts runId and parses terminal evidence counts', () async {
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
          'session': {'id': 'run-1', 'previewId': 'prev-1', 'state': 'succeeded'},
          'evidence': {
            'sessionId': 'run-1',
            'urlPath': '/?tab=home',
            'assertions': [
              {'name': 'title', 'passed': true},
              {'name': 'button', 'passed': false, 'detail': 'missing'},
            ],
            'consoleErrors': [
              {'sequence': 1, 'level': 'error', 'text': 'boom'},
              {'sequence': 2, 'level': 'error', 'text': 'again'},
            ],
            'screenshotId': 'shot-1',
          },
        }),
      );
      await request.response.close();
    });
    final http = LanHttpClient();
    addTearDown(http.close);
    final client = BrowserClient(http, 'http://127.0.0.1:${server.port}');
    final run = await client.getVerification(runId: 'run-1');
    expect(path, '/api/workbench/browser-verification/get');
    expect(body, {'runId': 'run-1'});
    expect(run.session.state, 'succeeded');
    expect(run.evidence?.urlPath, '/?tab=home');
    expect(run.evidence?.consoleErrorCount, 2);
    expect(run.evidence?.assertions, hasLength(2));
    expect(run.evidence?.assertionFailedCount, 1);
    expect(run.evidence?.screenshotId, 'shot-1');
  });

  test('getVerificationArtifact posts runId and artifactId', () async {
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
          'runId': 'run-1',
          'artifactId': 'shot-1',
          'contentType': 'image/png',
          'byteLen': 4,
          'base64': 'AAAA',
        }),
      );
      await request.response.close();
    });
    final http = LanHttpClient();
    addTearDown(http.close);
    final client = BrowserClient(http, 'http://127.0.0.1:${server.port}');
    final artifact = await client.getVerificationArtifact(
      runId: 'run-1',
      artifactId: 'shot-1',
    );
    expect(path, '/api/workbench/browser-verification/artifact');
    expect(body, {'runId': 'run-1', 'artifactId': 'shot-1'});
    expect(artifact.contentType, 'image/png');
    expect(artifact.base64, 'AAAA');
  });
}
