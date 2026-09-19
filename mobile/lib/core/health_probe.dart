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
