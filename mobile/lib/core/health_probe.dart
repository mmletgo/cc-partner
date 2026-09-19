import 'dart:async';
import 'dart:io';

import '../address_book/models.dart';
import 'lan_http.dart';

/// Probe `GET /api/health` on a LAN PC.
Future<HealthSnapshot> probeLanHealth(
  LanHttpClient http,
  String baseUrl, {
  Duration timeout = const Duration(seconds: 4),
}) async {
  final body = await http.getJson(baseUrl, '/api/health').timeout(timeout);
  final caps = body['capabilities'];
  return HealthSnapshot(
    ok: body['ok'] == true,
    deviceId: body['device_id'] as String?,
    deviceName: body['device_name'] as String?,
    protocolVersion: body['protocol_version'] as int?,
    capabilities: caps is List
        ? caps.map((e) => e.toString()).toList()
        : const [],
  );
}

/// Turn a probe failure into a short address-book label.
///
/// Business Logic: 扫码强制保存时必须告诉用户为什么仍是离线，不能只写「离线」。
/// Code Logic: 识别超时/套接字/HTTP/格式错误；其余用 toString，并去掉 `Exception: ` 前缀。
String describeProbeError(Object error) {
  if (error is TimeoutException) {
    return '探测超时';
  }
  if (error is SocketException) {
    return '连不上这台电脑';
  }
  if (error is LanHttpException) {
    return 'HTTP ${error.statusCode}';
  }
  if (error is FormatException) {
    return '健康检查响应无效';
  }
  var text = error.toString().trim();
  const prefix = 'Exception: ';
  if (text.startsWith(prefix)) {
    text = text.substring(prefix.length);
  }
  return text.isEmpty ? '探测失败' : text;
}
