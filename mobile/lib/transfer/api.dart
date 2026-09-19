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

  /// Business Logic: 失败/取消后的任务重新传输必须带稳定 clientOperationId，
  /// 网络异常后才能靠同一 id 对账，不能盲重放。
  /// Code Logic: POST /api/mobile/transfer/retry，body `{taskId, clientOperationId}`。
  Future<void> retry(String taskId, String clientOperationId) async {
    await _http.postJson(
      baseUrl,
      '/api/mobile/transfer/retry',
      {'taskId': taskId, 'clientOperationId': clientOperationId},
    );
  }

  /// Business Logic: 有续传元数据且对端支持 resume.v1 时继续传输，复用同一幂等键。
  /// Code Logic: POST /api/mobile/transfer/resume，body `{taskId, clientOperationId}`。
  Future<void> resume(String taskId, String clientOperationId) async {
    await _http.postJson(
      baseUrl,
      '/api/mobile/transfer/resume',
      {'taskId': taskId, 'clientOperationId': clientOperationId},
    );
  }

  /// Business Logic: timeout/network 后结果未知，必须先对账再决定成功或报错。
  /// Code Logic: POST /api/mobile/transfer/get-operation，body `{clientOperationId}`，
  /// 返回 `{status: notFound|pending|succeeded|failed, taskId?, code?}` 的宽容解析。
  Future<TransferOperationStatus> getOperation(String clientOperationId) async {
    final body = await _http.postJson(
      baseUrl,
      '/api/mobile/transfer/get-operation',
      {'clientOperationId': clientOperationId},
    );
    return TransferOperationStatus.fromJson(body);
  }
}

/// get-operation 对账结果（与 web TransferOperationStatus 联合对齐）。
class TransferOperationStatus {
  TransferOperationStatus({required this.status, this.taskId, this.code});

  /// notFound / pending / succeeded / failed。
  final String status;

  /// succeeded 时附带的任务 id。
  final String? taskId;

  /// failed 时附带的错误 code。
  final String? code;

  factory TransferOperationStatus.fromJson(Map<String, dynamic> json) {
    return TransferOperationStatus(
      status: json['status'] as String? ?? 'pending',
      taskId: json['taskId'] as String?,
      code: json['code'] as String?,
    );
  }

  /// Business Logic: pending 表示主机还没落终态，调用方应继续等待或提示稍后重试。
  bool get isPending => status == 'pending';
}

/// 与 web 对账节奏一致：最多 12 次、每次间隔 1.5s。
const transferReconcileMaxAttempts = 12;
const transferReconcileDelay = Duration(milliseconds: 1500);

/// 对账等待的可注入延时签名（测试用 no-op）。
typedef TransferReconcileDelay = Future<void> Function(Duration duration);

Future<void> _defaultReconcileDelay(Duration duration) => Future<void>.delayed(duration);

/// Business Logic: timeout/network 后只能用同一 clientOperationId 有界轮询对账，
/// 禁止立即 mint 新 id 盲重试。
/// Code Logic: 最多 maxAttempts 次调用 getOperation；非 pending 即返回终态；
/// 全程仍 pending 则返回最后的 pending 状态；getOperation 抛错向上传播由调用方处理。
Future<TransferOperationStatus?> reconcileTransferOperation({
  required TransferApi api,
  required String clientOperationId,
  int maxAttempts = transferReconcileMaxAttempts,
  TransferReconcileDelay delay = _defaultReconcileDelay,
}) async {
  for (var attempt = 0; attempt < maxAttempts; attempt++) {
    final status = await api.getOperation(clientOperationId);
    if (!status.isPending) {
      return status;
    }
    if (attempt + 1 >= maxAttempts) {
      return status;
    }
    await delay(transferReconcileDelay);
  }
  return TransferOperationStatus(status: 'pending');
}

/// Business Logic: timeout/network 后结果未知，不得把错误当确定性失败去重发；
/// Code Logic: 匹配错误文案/类型中的 timeout/network/offline/unavailable 关键词，
/// 与 web isTransferOutcomeUncertain 同语义。
bool isTransferOutcomeUncertain(Object error) {
  final String code;
  if (error is LanHttpException) {
    code = '${error.statusCode}';
  } else {
    code = '';
  }
  final message = error.toString().toLowerCase();
  final hay = '$code $message';
  return hay.contains('timeout') ||
      hay.contains('network') ||
      hay.contains('offline') ||
      hay.contains('unavailable');
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
