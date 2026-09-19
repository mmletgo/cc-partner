import 'dart:convert';
import 'dart:io';

/// LAN HTTP client for a cc-partner PC.
///
/// Business Logic: native peers omit Origin; Host must be the PC host:port.
/// Code Logic: dart:io HttpClient never adds Origin. URI is `{baseUrl}{path}`.
class LanHttpClient {
  LanHttpClient({HttpClient? client}) : _client = client ?? HttpClient();

  final HttpClient _client;

  void close() => _client.close(force: true);

  Future<Map<String, dynamic>> getJson(String baseUrl, String path) async {
    final uri = _uri(baseUrl, path);
    final request = await _client.getUrl(uri);
    _stripOrigin(request);
    final response = await request.close();
    return _decode(response);
  }

  Future<Map<String, dynamic>> postJson(
    String baseUrl,
    String path,
    Map<String, dynamic> body,
  ) async {
    final uri = _uri(baseUrl, path);
    final request = await _client.postUrl(uri);
    _stripOrigin(request);
    request.headers.contentType = ContentType.json;
    request.add(utf8.encode(jsonEncode(body)));
    final response = await request.close();
    return _decode(response);
  }

  Uri _uri(String baseUrl, String path) {
    final normalized = baseUrl.endsWith('/')
        ? baseUrl.substring(0, baseUrl.length - 1)
        : baseUrl;
    final suffix = path.startsWith('/') ? path : '/$path';
    return Uri.parse('$normalized$suffix');
  }

  void _stripOrigin(HttpClientRequest request) {
    request.headers.removeAll('origin');
    request.headers.removeAll('Origin');
  }

  Future<Map<String, dynamic>> _decode(HttpClientResponse response) async {
    final text = await utf8.decodeStream(response);
    if (response.statusCode < 200 || response.statusCode >= 300) {
      throw LanHttpException(response.statusCode, text);
    }
    if (text.isEmpty) {
      return <String, dynamic>{};
    }
    final decoded = jsonDecode(text);
    if (decoded is Map<String, dynamic>) {
      return decoded;
    }
    throw LanHttpException(response.statusCode, 'expected JSON object');
  }
}

class LanHttpException implements Exception {
  LanHttpException(this.statusCode, this.body);
  final int statusCode;
  final String body;
  @override
  String toString() => 'LAN HTTP $statusCode: $body';
}
