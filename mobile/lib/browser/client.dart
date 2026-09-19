import '../core/lan_http.dart';

/// Live Browser workspace preview: scripts on, distinct from HTML file sandbox.
class BrowserPreviewPolicy {
  static const scriptsEnabled = true;

  static String proxyPath(String previewId) =>
      '/api/mobile/workbench/browser/proxy/$previewId/';
}

class BrowserPreview {
  const BrowserPreview({
    required this.previewId,
    required this.mobileProxyPath,
    this.targetUrl,
    this.scriptsEnabled = true,
  });

  final String previewId;
  final String mobileProxyPath;
  final String? targetUrl;
  final bool scriptsEnabled;
}

class BrowserClient {
  BrowserClient(this._http, this.baseUrl);

  final LanHttpClient _http;
  final String baseUrl;

  Future<Map<String, dynamic>> discover({
    required String projectId,
    String? worktreeId,
  }) {
    return _http.postJson(
      baseUrl,
      '/api/mobile/workbench/browser/discover',
      {
        'projectId': projectId,
        'worktreeId': worktreeId,
      },
    );
  }

  Future<BrowserPreview> createPreview({
    required String projectId,
    String? worktreeId,
    required String targetUrl,
  }) async {
    final body = await _http.postJson(
      baseUrl,
      '/api/mobile/workbench/browser/preview',
      {
        'projectId': projectId,
        'worktreeId': worktreeId,
        'targetUrl': targetUrl,
      },
    );
    final previewId = body['previewId'] as String? ?? '';
    return BrowserPreview(
      previewId: previewId,
      mobileProxyPath: body['mobileProxyPath'] as String? ??
          BrowserPreviewPolicy.proxyPath(previewId),
      targetUrl: body['targetUrl'] as String? ?? targetUrl,
      scriptsEnabled: true,
    );
  }
}
