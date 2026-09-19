import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

import '../core/lan_http.dart';
import 'client.dart';

class TransferApi {
  TransferApi(this._http, this.baseUrl);

  final LanHttpClient _http;
  final String baseUrl;

  Future<List<TransferTask>> listTasks() async {
    final body = await _http.getDynamic(baseUrl, '/api/mobile/transfer/tasks');
    return asObjectList(body, wrapKey: 'tasks').map(TransferTask.fromJson).toList();
  }

  Future<List<Map<String, dynamic>>> listDevices() async {
    final body = await _http.getDynamic(baseUrl, '/api/mobile/devices');
    return asObjectList(body, wrapKey: 'devices');
  }

  Future<Map<String, dynamic>> uploadInit({
    required String filename,
    required int size,
    required String deviceId,
    required String clientOperationId,
  }) {
    return _http.postJson(
      baseUrl,
      '/api/mobile/transfer/upload/init',
      {
        'filename': filename,
        'size': size,
        'deviceId': deviceId,
        'clientOperationId': clientOperationId,
      },
    );
  }

  Future<void> uploadChunk(String id, int offset, Uint8List bytes) async {
    final uriBase = baseUrl.endsWith('/') ? baseUrl.substring(0, baseUrl.length - 1) : baseUrl;
    final client = HttpClient();
    try {
      final request = await client.postUrl(
        Uri.parse('$uriBase/api/mobile/transfer/upload/chunk/$id?offset=$offset'),
      );
      request.headers.removeAll('origin');
      request.headers.contentType = ContentType.binary;
      request.add(bytes);
      final response = await request.close();
      await utf8.decodeStream(response);
      if (response.statusCode < 200 || response.statusCode >= 300) {
        throw LanHttpException(response.statusCode, 'chunk failed');
      }
    } finally {
      client.close(force: true);
    }
  }

  Future<void> uploadComplete(String id) async {
    await _http.postJson(
      baseUrl,
      '/api/mobile/transfer/upload/complete/$id',
      const {},
    );
  }
}

String newClientOperationId() =>
    'mob-${DateTime.now().microsecondsSinceEpoch}-${DateTime.now().millisecondsSinceEpoch % 997}';
