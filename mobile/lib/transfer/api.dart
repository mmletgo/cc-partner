import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

import '../core/lan_http.dart';
import 'client.dart';

/// 分块大小，与 /mobile 的 MOBILE_TRANSFER_CHUNK_SIZE 对齐。
const transferChunkSize = 256 * 1024;

/// 单块上传动作签名；测试可注入假发送器代替真实网络。
typedef TransferChunkSender = Future<void> Function(
  String id,
  int offset,
  Uint8List bytes,
);

/// 分块上传进度回调：uploadedBytes 为累计已发送字节，totalBytes 为总字节。
typedef TransferProgressCallback = void Function(int uploadedBytes, int totalBytes);

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

  /// 分块上传整份文件并回报累计进度；chunkSender 可注入（测试假发送器）。
  ///
  /// 回调序列：先报 (0, total)，之后每成功一块报一次累计值；空文件只报 (0, 0)。
  Future<void> uploadFileInChunks({
    required String id,
    required Uint8List bytes,
    required TransferProgressCallback onProgress,
    TransferChunkSender? chunkSender,
    int chunkSize = transferChunkSize,
  }) async {
    final send = chunkSender ?? uploadChunk;
    final total = bytes.length;
    onProgress(0, total);
    for (var offset = 0; offset < total; offset += chunkSize) {
      final end = (offset + chunkSize > total) ? total : offset + chunkSize;
      await send(id, offset, bytes.sublist(offset, end));
      onProgress(end, total);
    }
  }

  bool get allowsBlindChunkRetry => false;

  Future<List<int>> download(String taskId) {
    return _http.getBytes(
      baseUrl,
      '/api/mobile/transfer/download/$taskId',
    );
  }

  Future<void> cancel(String taskId) async {
    await _http.postJson(
      baseUrl,
      '/api/mobile/transfer/cancel',
      {'taskId': taskId},
    );
  }
}

String newClientOperationId() =>
    'mob-${DateTime.now().microsecondsSinceEpoch}-${DateTime.now().millisecondsSinceEpoch % 997}';

typedef TransferSaveSink = Future<void> Function({
  required String fileName,
  required List<int> bytes,
});

/// Fetch the task bytes from the host relay, then persist them on the phone.
Future<List<int>> downloadAndSaveTask({
  required TransferApi api,
  required String taskId,
  required String fileName,
  required TransferSaveSink save,
}) async {
  final bytes = await api.download(taskId);
  await save(fileName: fileName, bytes: bytes);
  return bytes;
}
