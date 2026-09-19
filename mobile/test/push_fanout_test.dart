import 'dart:convert';
import 'dart:io';

import 'package:cc_partner_mobile/address_book/book.dart';
import 'package:cc_partner_mobile/address_book/models.dart';
import 'package:cc_partner_mobile/core/lan_http.dart';
import 'package:cc_partner_mobile/push/fanout.dart';
import 'package:test/test.dart';

void main() {
  test('registers token on every online PC that advertises mobile.push.v1', () async {
    final registered = <String>[];
    final server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    addTearDown(() => server.close(force: true));
    server.listen((request) async {
      registered.add(request.uri.path);
      await utf8.decodeStream(request);
      request.response.headers.contentType = ContentType.json;
      request.response.write('{"ok":true}');
      await request.response.close();
    });
    final base = 'http://127.0.0.1:${server.port}';
    final book = AddressBook(store: MemoryAddressBookStore(), mobileDeviceId: 'phone-x');
    await book.addFromInput(
      base,
      probe: (_) async => const HealthSnapshot(
        ok: true,
        deviceId: 'pc-1',
        capabilities: [kMobilePushCapability],
      ),
    );
    await book.addFromInput(
      '10.0.0.9:62116',
      probe: (_) async => throw Exception('offline'),
      forceIfUnreachable: true,
    );
    final http = LanHttpClient();
    addTearDown(http.close);
    final errors = await PushFanout(http).registerAll(
      book: book,
      token: 'tok',
      platform: 'ios',
      appBuild: '1',
    );
    expect(errors, isEmpty);
    expect(registered, ['/api/mobile/push/register']);
    expect(book.servers.first.pushIntent?.registeredToken, 'tok');
  });
}
