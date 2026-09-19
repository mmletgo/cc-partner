import 'dart:convert';
import 'dart:io';

import 'package:cc_partner_mobile/core/lan_http.dart';
import 'package:cc_partner_mobile/provider/client.dart';
import 'package:test/test.dart';

void main() {
  test('phone never installs cc-switch CLI', () {
    expect(ProviderClient(LanHttpClient(), 'http://127.0.0.1:1').allowsPhoneCliInstall, isFalse);
  });

  test('missing capability is unsupported', () async {
    final server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    addTearDown(() => server.close(force: true));
    server.listen((request) async {
      request.response.headers.contentType = ContentType.json;
      request.response.write(
        jsonEncode({
          'ok': true,
          'protocol_version': 1,
          'capabilities': ['attention.v2'],
        }),
      );
      await request.response.close();
    });
    final http = LanHttpClient();
    addTearDown(http.close);
    final client = ProviderClient(http, 'http://127.0.0.1:${server.port}');
    expect(await client.probe(), ProviderSupport.unsupported);
  });

  test('provider-manager.v1 is ready', () async {
    final server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    addTearDown(() => server.close(force: true));
    server.listen((request) async {
      request.response.headers.contentType = ContentType.json;
      request.response.write(
        jsonEncode({
          'ok': true,
          'protocol_version': 1,
          'capabilities': ['provider-manager.v1'],
        }),
      );
      await request.response.close();
    });
    final http = LanHttpClient();
    addTearDown(http.close);
    final client = ProviderClient(http, 'http://127.0.0.1:${server.port}');
    expect(await client.probe(), ProviderSupport.ready);
  });

  test('switch posts app and providerId and never calls install-cli', () async {
    final server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    addTearDown(() => server.close(force: true));
    final paths = <String>[];
    Map<String, dynamic>? switchBody;
    server.listen((request) async {
      paths.add(request.uri.path);
      final raw = await utf8.decodeStream(request);
      if (request.uri.path.endsWith('/switch') && raw.isNotEmpty) {
        switchBody = jsonDecode(raw) as Map<String, dynamic>;
      }
      request.response.headers.contentType = ContentType.json;
      if (request.uri.path.endsWith('/summary')) {
        request.response.write(
          jsonEncode({
            'apps': [
              {
                'app': 'claude',
                'currentProviderId': 'p-a',
                'providers': [
                  {'id': 'p-a', 'name': 'A', 'isCurrent': true},
                  {'id': 'p-b', 'name': 'B', 'isCurrent': false},
                ],
              },
            ],
          }),
        );
      } else {
        request.response.write(jsonEncode({'ok': true}));
      }
      await request.response.close();
    });
    final http = LanHttpClient();
    addTearDown(http.close);
    final client = ProviderClient(http, 'http://127.0.0.1:${server.port}');
    final summary = await client.summary();
    expect(client.appsFromSummary(summary).single.app, 'claude');
    await client.switchProvider(app: 'claude', providerId: 'p-b');
    expect(paths, isNot(contains(ProviderClient.installCliPath)));
    expect(switchBody, {'app': 'claude', 'providerId': 'p-b'});
  });
}
