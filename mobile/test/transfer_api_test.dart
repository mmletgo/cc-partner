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

  test('chunked upload reports cumulative progress through the injectable sender', () async {
    final api = TransferApi(LanHttpClient(), 'http://127.0.0.1:1');
    final progress = <List<int>>[];
    final chunks = <List<int>>[];
    final bytes = Uint8List.fromList(List<int>.generate(3072, (i) => i % 256));
    await api.uploadFileInChunks(
      id: 'up-1',
      bytes: bytes,
      chunkSize: 1024,
      onProgress: (uploaded, total) => progress.add([uploaded, total]),
      chunkSender: (id, offset, chunk) async {
        expect(id, 'up-1');
        chunks.add([offset, chunk.length]);
      },
    );
    expect(chunks, [
      [0, 1024],
      [1024, 1024],
      [2048, 1024],
    ]);
    expect(progress, [
      [0, 3072],
      [1024, 3072],
      [2048, 3072],
      [3072, 3072],
    ]);
  });

  test('empty upload reports a single zero progress and sends no chunks', () async {
    final api = TransferApi(LanHttpClient(), 'http://127.0.0.1:1');
    final progress = <List<int>>[];
    var chunkCalls = 0;
    await api.uploadFileInChunks(
      id: 'up-0',
      bytes: Uint8List(0),
      onProgress: (uploaded, total) => progress.add([uploaded, total]),
      chunkSender: (_, __, ___) async => chunkCalls += 1,
    );
    expect(progress, [
      [0, 0],
    ]);
    expect(chunkCalls, 0);
  });

  test('default chunk sender posts each chunk to the host relay route', () async {
    final server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    addTearDown(() => server.close(force: true));
    final offsets = <int>[];
    server.listen((request) async {
      if (request.uri.path.contains('/upload/chunk/')) {
        offsets.add(int.parse(request.uri.queryParameters['offset'] ?? '-1'));
      }
      await utf8.decodeStream(request);
      request.response.headers.contentType = ContentType.json;
      request.response.write(jsonEncode({'ok': true}));
      await request.response.close();
    });
    final http = LanHttpClient();
    addTearDown(http.close);
    final api = TransferApi(http, 'http://127.0.0.1:${server.port}');
    await api.uploadFileInChunks(
      id: 'up-2',
      bytes: Uint8List(3000),
      chunkSize: 1024,
      onProgress: (_, __) {},
    );
    expect(offsets, [0, 1024, 2048]);
  });

  test('retry/resume/get-operation hit host-relay routes with idempotency key', () async {
    final server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    addTearDown(() => server.close(force: true));
    final bodies = <String, Map<String, dynamic>>{};
    var operationCalls = 0;
    server.listen((request) async {
      final raw = await utf8.decodeStream(request);
      final body = raw.isEmpty ? <String, dynamic>{} : jsonDecode(raw) as Map<String, dynamic>;
      bodies['${request.method} ${request.uri.path}'] = body;
      if (request.uri.path.endsWith('/get-operation')) {
        operationCalls += 1;
        // 前两次仍 pending，第三次确认成功。
        final status = operationCalls < 3 ? 'pending' : 'succeeded';
        request.response.headers.contentType = ContentType.json;
        request.response.write(jsonEncode({'status': status, if (status == 'succeeded') 'taskId': 't-9'}));
      } else {
        request.response.headers.contentType = ContentType.json;
        request.response.write(jsonEncode({'ok': true}));
      }
      await request.response.close();
    });
    final http = LanHttpClient();
    addTearDown(http.close);
    final api = TransferApi(http, 'http://127.0.0.1:${server.port}');
    await api.retry('t-9', 'op-1');
    await api.resume('t-9', 'op-2');
    expect(bodies['POST /api/mobile/transfer/retry'], {'taskId': 't-9', 'clientOperationId': 'op-1'});
    expect(bodies['POST /api/mobile/transfer/resume'], {'taskId': 't-9', 'clientOperationId': 'op-2'});

    // 对账：pending → pending → succeeded，间隔用 no-op。
    var delayCalls = 0;
    final status = await reconcileTransferOperation(
      api: api,
      clientOperationId: 'op-3',
      delay: (_) async => delayCalls++,
    );
    expect(status?.status, 'succeeded');
    expect(status?.taskId, 't-9');
    expect(operationCalls, 3);
    expect(delayCalls, 2); // 终态前每次 pending 等待一个间隔。
    expect(
      bodies['POST /api/mobile/transfer/get-operation'],
      {'clientOperationId': 'op-3'},
    );
  });

  test('reconcile returns the trailing pending status after exhausting attempts', () async {
    final server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    addTearDown(() => server.close(force: true));
    var calls = 0;
    server.listen((request) async {
      await utf8.decodeStream(request);
      calls += 1;
      request.response.headers.contentType = ContentType.json;
      request.response.write(jsonEncode({'status': 'pending'}));
      await request.response.close();
    });
    final http = LanHttpClient();
    addTearDown(http.close);
    final api = TransferApi(http, 'http://127.0.0.1:${server.port}');
    final status = await reconcileTransferOperation(
      api: api,
      clientOperationId: 'op-slow',
      maxAttempts: 3,
      delay: (_) async {},
    );
    expect(calls, 3);
    expect(status?.status, 'pending');
    expect(status?.isPending, isTrue);
  });

  test('failed operation status carries the code', () async {
    final server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    addTearDown(() => server.close(force: true));
    server.listen((request) async {
      await utf8.decodeStream(request);
      request.response.headers.contentType = ContentType.json;
      request.response.write(jsonEncode({'status': 'failed', 'code': 'peerUnreachable'}));
      await request.response.close();
    });
    final http = LanHttpClient();
    addTearDown(http.close);
    final api = TransferApi(http, 'http://127.0.0.1:${server.port}');
    final status = await api.getOperation('op-x');
    expect(status.status, 'failed');
    expect(status.code, 'peerUnreachable');
    expect(status.isPending, isFalse);
  });

  test('only timeout/network style errors are outcome-uncertain', () {
    expect(
      isTransferOutcomeUncertain(const SocketException('Network is unreachable')),
      isTrue,
    );
    expect(
      isTransferOutcomeUncertain(const SocketException('connection timeout, errno = 60')),
      isTrue,
    );
    expect(isTransferOutcomeUncertain(LanHttpException(0, 'request timeout')), isTrue);
    expect(isTransferOutcomeUncertain(LanHttpException(500, 'boom')), isFalse);
    expect(isTransferOutcomeUncertain(StateError('bad state')), isFalse);
  });
}
