import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

import 'package:cc_partner_mobile/core/lan_http.dart';
import 'package:cc_partner_mobile/transfer/api.dart';
import 'package:test/test.dart';

void main() {
  test('download and cancel hit host-relay routes and never retry a chunk blindly', () async {
    final server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    addTearDown(() => server.close(force: true));
    final paths = <String>[];
    Map<String, dynamic>? cancelBody;
    server.listen((request) async {
      paths.add('${request.method} ${request.uri.path}');
      final raw = await utf8.decodeStream(request);
      if (request.uri.path.endsWith('/cancel') && raw.isNotEmpty) {
        cancelBody = jsonDecode(raw) as Map<String, dynamic>;
      }
      if (request.uri.path.contains('/download/')) {
        request.response.headers.contentType = ContentType.binary;
        request.response.add([1, 2, 3]);
      } else {
        request.response.headers.contentType = ContentType.json;
        request.response.write(jsonEncode({'ok': true}));
      }
      await request.response.close();
    });
    final http = LanHttpClient();
    addTearDown(http.close);
    final api = TransferApi(http, 'http://127.0.0.1:${server.port}');
    final bytes = await api.download('t-recv');
    expect(bytes, Uint8List.fromList([1, 2, 3]));
    await api.cancel('t-up');
    expect(paths, contains('GET /api/mobile/transfer/download/t-recv'));
    expect(paths, contains('POST /api/mobile/transfer/cancel'));
    expect(cancelBody?['taskId'], 't-up');
    expect(api.allowsBlindChunkRetry, isFalse);
  });

  test('download path writes the fetched bytes to a save sink', () async {
    final server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    addTearDown(() => server.close(force: true));
    server.listen((request) async {
      request.response.headers.contentType = ContentType.binary;
      request.response.add([9, 8, 7]);
      await request.response.close();
    });
    final http = LanHttpClient();
    addTearDown(http.close);
    final api = TransferApi(http, 'http://127.0.0.1:${server.port}');
    String? savedName;
    List<int>? savedBytes;
    final returned = await downloadAndSaveTask(
      api: api,
      taskId: 't-recv',
      fileName: 'notes.txt',
      save: ({required fileName, required bytes}) async {
        savedName = fileName;
        savedBytes = List<int>.from(bytes);
      },
    );
    expect(returned, [9, 8, 7]);
    expect(savedName, 'notes.txt');
    expect(savedBytes, [9, 8, 7]);
  });
}
