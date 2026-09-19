import 'dart:convert';
import 'dart:io';

import 'package:cc_partner_mobile/core/lan_http.dart';
import 'package:test/test.dart';

void main() {
  late HttpServer server;
  late Uri base;
  Map<String, String>? lastHeaders;

  setUp(() async {
    lastHeaders = null;
    server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    base = Uri(scheme: 'http', host: '127.0.0.1', port: server.port);
    server.listen((request) async {
      lastHeaders = {
        'host': request.headers.value('host') ?? '',
        'origin': request.headers.value('origin') ?? '',
      };
      request.response.headers.contentType = ContentType.json;
      request.response.write(
        jsonEncode({
          'ok': true,
          'device_id': 'dev-1',
          'device_name': 'Test PC',
          'http_port': server.port,
          'protocol_version': 1,
          'capabilities': ['attention.v2'],
        }),
      );
      await request.response.close();
    });
  });

  tearDown(() async {
    await server.close(force: true);
  });

  test('omits Origin and sets Host from baseUrl', () async {
    final client = LanHttpClient();
    addTearDown(client.close);
    final body = await client.getJson(base.toString(), '/api/health');
    expect(body['ok'], isTrue);
    expect(body['device_id'], 'dev-1');
    expect(lastHeaders, isNotNull);
    expect(lastHeaders!['origin'], isEmpty);
    expect(lastHeaders!['host'], '127.0.0.1:${server.port}');
  });
}
