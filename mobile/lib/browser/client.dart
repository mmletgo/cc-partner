import '../core/lan_http.dart';

/// Live Browser workspace preview: scripts on, distinct from HTML file sandbox.
class BrowserPreviewPolicy {
  static const scriptsEnabled = true;

  static String proxyPath(String previewId) =>
      '/api/mobile/workbench/browser/proxy/$previewId/';
}

/// discover 返回的单个 dev server 候选（对齐 web WorkbenchBrowserTarget）。
class BrowserTarget {
  const BrowserTarget({
    required this.id,
    required this.url,
    this.displayUrl,
    this.source,
    this.reachable = false,
  });

  final String id;
  final String url;
  final String? displayUrl;
  final String? source;
  final bool reachable;

  factory BrowserTarget.fromJson(Map<String, dynamic> json) => BrowserTarget(
        id: json['id'] as String? ?? '',
        url: json['url'] as String? ?? '',
        displayUrl: json['displayUrl'] as String?,
        source: json['source'] as String?,
        reachable: json['reachable'] == true,
      );

  /// chip 上显示的短地址；后端缺 displayUrl 时退回完整 url。
  String get label =>
      (displayUrl != null && displayUrl!.isNotEmpty) ? displayUrl! : url;
}

/// discover 结果（对齐 web WorkbenchBrowserDiscovery）。
class BrowserDiscovery {
  const BrowserDiscovery({
    required this.targets,
    this.selectedTargetId,
  });

  final List<BrowserTarget> targets;
  final String? selectedTargetId;

  factory BrowserDiscovery.fromJson(Map<String, dynamic> json) =>
      BrowserDiscovery(
        targets: asObjectList(json['targets'])
            .map(BrowserTarget.fromJson)
            .toList(),
        selectedTargetId: json['selectedTargetId'] as String?,
      );

  /// 后端推荐的默认候选（仅指向可达的 remembered/terminalOutput/projectConfig）。
  BrowserTarget? get selectedTarget {
    final id = selectedTargetId;
    if (id == null || id.isEmpty) {
      return null;
    }
    for (final target in targets) {
      if (target.id == id) {
        return target;
      }
    }
    return null;
  }
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

  /// 探测项目/worktree 下的 dev server 候选；失败由调用方决定静默降级。
  Future<BrowserDiscovery> discover({
    required String projectId,
    String? worktreeId,
  }) async {
    final body = await _http.postJson(
      baseUrl,
      '/api/mobile/workbench/browser/discover',
      {
        'projectId': projectId,
        'worktreeId': worktreeId,
      },
    );
    return BrowserDiscovery.fromJson(body);
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
