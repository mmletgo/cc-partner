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
}
