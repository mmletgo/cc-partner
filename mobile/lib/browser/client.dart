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

/// 浏览器验证会话（对齐 web BrowserVerificationSession，camelCase 宽容解析）。
class BrowserVerificationSession {
  const BrowserVerificationSession({
    required this.id,
    required this.previewId,
    required this.state,
    this.errorCode,
    this.errorMessage,
  });

  final String id;
  final String previewId;

  /// queued/running/succeeded/failed/canceled；后端缺省按 running 继续轮询。
  final String state;
  final String? errorCode;
  final String? errorMessage;

  /// Business Logic: 一键验证需要展示 run 状态与错误码，字段缺失不能让 UI 崩溃。
  /// Code Logic: 宽容解析 id/previewId/state 与可选错误字段。
  factory BrowserVerificationSession.fromJson(Map<String, dynamic> json) =>
      BrowserVerificationSession(
        id: json['id'] as String? ?? '',
        previewId: json['previewId'] as String? ?? '',
        state: json['state'] as String? ?? 'running',
        errorCode: json['errorCode'] as String?,
        errorMessage: json['errorMessage'] as String?,
      );
}

/// 浏览器验证 evidence 摘要（对齐 web BrowserVerificationEvidence）。
class BrowserVerificationEvidence {
  const BrowserVerificationEvidence({
    this.urlPath,
    this.assertions = const [],
    this.consoleErrorCount = 0,
    this.screenshotId,
  });

  final String? urlPath;
  final List<BrowserVerificationAssertion> assertions;
  final int consoleErrorCount;
  final String? screenshotId;

  /// Business Logic: 摘要卡需要断言失败数；后端只给逐条 passed 布尔。
  /// Code Logic: 统计 passed != true 的断言条数。
  int get assertionFailedCount =>
      assertions.where((a) => !a.passed).length;

  /// Business Logic: UI 展示 a11y/console/screenshot 摘要，缺字段按 0 条处理。
  /// Code Logic: 宽容解析 urlPath/assertions/consoleErrors/screenshotId；
  /// consoleErrors 只取条数（内容已脱敏且移动端摘要不逐条展示）。
  factory BrowserVerificationEvidence.fromJson(Map<String, dynamic> json) {
    final rawConsole = json['consoleErrors'];
    return BrowserVerificationEvidence(
      urlPath: json['urlPath'] as String?,
      assertions: asObjectList(json['assertions'])
          .map(BrowserVerificationAssertion.fromJson)
          .toList(),
      consoleErrorCount: rawConsole is List ? rawConsole.length : 0,
      screenshotId: json['screenshotId'] as String?,
    );
  }
}

/// 单条断言结果（对齐 web evidence.assertions 元素）。
class BrowserVerificationAssertion {
  const BrowserVerificationAssertion({
    required this.name,
    required this.passed,
    this.detail,
  });

  final String name;
  final bool passed;
  final String? detail;

  /// Business Logic: 摘要需要区分断言通过/失败。
  /// Code Logic: 宽容解析 name/passed/detail。
  factory BrowserVerificationAssertion.fromJson(Map<String, dynamic> json) =>
      BrowserVerificationAssertion(
        name: json['name'] as String? ?? '',
        passed: json['passed'] == true,
        detail: json['detail'] as String?,
      );
}

/// 浏览器验证 run（对齐 web BrowserVerificationRun：session + 可选 evidence）。
class BrowserVerificationRun {
  const BrowserVerificationRun({
    required this.session,
    this.evidence,
  });

  final BrowserVerificationSession session;
  final BrowserVerificationEvidence? evidence;

  /// Business Logic: start/get 都返回完整 run；evidence 只在结束时出现。
  /// Code Logic: 宽容解析 session 与可选 evidence。
  factory BrowserVerificationRun.fromJson(Map<String, dynamic> json) {
    final evidence = json['evidence'];
    return BrowserVerificationRun(
      session: BrowserVerificationSession.fromJson(
        asObject(json['session'] ?? const <String, dynamic>{}),
      ),
      evidence: evidence is Map
          ? BrowserVerificationEvidence.fromJson(Map<String, dynamic>.from(evidence))
          : null,
    );
  }
}

/// 浏览器验证 artifact（对齐 web BrowserVerificationArtifact：截图 PNG base64）。
class BrowserVerificationArtifact {
  const BrowserVerificationArtifact({
    required this.runId,
    required this.artifactId,
    required this.contentType,
    required this.base64,
  });

  final String runId;
  final String artifactId;
  final String contentType;
  final String base64;

  /// Business Logic: 移动端与 web mobile surface 一样展示验证截图。
  /// Code Logic: 宽容解析 runId/artifactId/contentType/base64。
  factory BrowserVerificationArtifact.fromJson(Map<String, dynamic> json) =>
      BrowserVerificationArtifact(
        runId: json['runId'] as String? ?? '',
        artifactId: json['artifactId'] as String? ?? '',
        contentType: json['contentType'] as String? ?? 'image/png',
        base64: json['base64'] as String? ?? '',
      );
}

/// Business Logic: UI 轮询需要知道 run 是否已结束（对齐 web 同名 helper）。
/// Code Logic: state 为 succeeded/failed/canceled 即终态。
bool isBrowserVerificationTerminalState(String state) =>
    state == 'succeeded' || state == 'failed' || state == 'canceled';

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

  /// Business Logic: 一键验证当前 live preview（默认 smoke），对齐 web
  /// transport.browser.startVerification；手机端经同源 P2P owner 路由。
  /// Code Logic: POST `/api/workbench/browser-verification/create`
  /// `{previewId, requestId}`，返回完整 BrowserVerificationRun。
  Future<BrowserVerificationRun> startVerification({
    required String previewId,
    required String requestId,
  }) async {
    final body = await _http.postJson(
      baseUrl,
      '/api/workbench/browser-verification/create',
      {
        'previewId': previewId,
        'requestId': requestId,
      },
    );
    return BrowserVerificationRun.fromJson(body);
  }

  /// Business Logic: 验证是异步 run，UI 需要有界轮询拿到最新状态与 evidence。
  /// Code Logic: POST `/api/workbench/browser-verification/get` `{runId}`。
  Future<BrowserVerificationRun> getVerification({required String runId}) async {
    final body = await _http.postJson(
      baseUrl,
      '/api/workbench/browser-verification/get',
      {'runId': runId},
    );
    return BrowserVerificationRun.fromJson(body);
  }

  /// Business Logic: 验证成功后展示截图（web mobile surface 同样展示）。
  /// Code Logic: POST `/api/workbench/browser-verification/artifact`
  /// `{runId, artifactId}`，返回 PNG base64。
  Future<BrowserVerificationArtifact> getVerificationArtifact({
    required String runId,
    required String artifactId,
  }) async {
    final body = await _http.postJson(
      baseUrl,
      '/api/workbench/browser-verification/artifact',
      {
        'runId': runId,
        'artifactId': artifactId,
      },
    );
    return BrowserVerificationArtifact.fromJson(body);
  }
}
