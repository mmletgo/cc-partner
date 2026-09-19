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
    return asObject(await getDynamic(baseUrl, path));
  }

  Future<dynamic> getDynamic(String baseUrl, String path) async {
    final uri = _uri(baseUrl, path);
    final request = await _client.getUrl(uri);
    _stripOrigin(request);
    final response = await request.close();
    return _decodeDynamic(response);
  }

  Future<Map<String, dynamic>> postJson(
    String baseUrl,
    String path,
    Map<String, dynamic> body,
  ) async {
    return asObject(await postDynamic(baseUrl, path, body));
  }

  Future<dynamic> postDynamic(
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
    return _decodeDynamic(response);
  }

  Stream<String> streamLines(String baseUrl, String path) async* {
    final uri = _uri(baseUrl, path);
    final request = await _client.getUrl(uri);
    _stripOrigin(request);
    request.headers.set(HttpHeaders.acceptHeader, 'application/x-ndjson, application/json');
    final response = await request.close();
    if (response.statusCode < 200 || response.statusCode >= 300) {
      final text = await utf8.decodeStream(response);
      throw LanHttpException(response.statusCode, text);
    }
    yield* response.transform(utf8.decoder).transform(const LineSplitter());
  }

  Future<WebSocket> openWebSocket(
    String baseUrl,
    String path, {
    Iterable<String>? protocols,
  }) {
    final http = _uri(baseUrl, path);
    final ws = http.replace(scheme: http.scheme == 'https' ? 'wss' : 'ws');
    return WebSocket.connect(ws.toString(), protocols: protocols);
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

  Future<dynamic> _decodeDynamic(HttpClientResponse response) async {
    final text = await utf8.decodeStream(response);
    if (response.statusCode < 200 || response.statusCode >= 300) {
      throw LanHttpException(response.statusCode, text);
    }
    if (text.isEmpty) {
      return <String, dynamic>{};
    }
    return jsonDecode(text);
  }
}

Map<String, dynamic> asObject(dynamic decoded) {
  if (decoded is Map<String, dynamic>) {
    return decoded;
  }
  if (decoded is Map) {
    return Map<String, dynamic>.from(decoded);
  }
  throw FormatException('expected JSON object');
}

List<Map<String, dynamic>> asObjectList(dynamic decoded, {String? wrapKey}) {
  if (decoded is List) {
    return decoded.map((e) => asObject(e)).toList();
  }
  if (decoded is Map && wrapKey != null && decoded[wrapKey] is List) {
    return (decoded[wrapKey] as List).map((e) => asObject(e)).toList();
  }
  return const [];
}

class LanHttpException implements Exception {
  LanHttpException(this.statusCode, this.body);
  final int statusCode;
  final String body;
  @override
  String toString() => 'LAN HTTP $statusCode: $body';
}
