import '../core/lan_http.dart';

class SessionSummary {
  const SessionSummary({
    required this.id,
    required this.projectId,
    required this.name,
    required this.status,
    this.worktreeId,
  });

  final String id;
  final String projectId;
  final String name;
  final String status;
  final String? worktreeId;

  factory SessionSummary.fromJson(Map<String, dynamic> json) => SessionSummary(
        id: json['id'] as String? ?? '',
        projectId: json['projectId'] as String? ?? '',
        name: json['name'] as String? ?? '',
        status: json['status'] as String? ?? '',
        worktreeId: json['worktreeId'] as String?,
      );
}

class SessionsClient {
  SessionsClient(this._http, this.baseUrl);

  final LanHttpClient _http;
  final String baseUrl;

  Future<List<SessionSummary>> list(String projectId) async {
    final body = await _http.postDynamic(
      baseUrl,
      '/api/mobile/workbench/sessions/list',
      {'projectId': projectId},
    );
    return asObjectList(body).map(SessionSummary.fromJson).toList();
  }

  Future<SessionSummary> create(String projectId, {String? worktreeId}) async {
    final body = await _http.postJson(
      baseUrl,
      '/api/mobile/workbench/sessions/create',
      {
        'projectId': projectId,
        if (worktreeId != null) 'worktreeId': worktreeId,
      },
    );
    return SessionSummary.fromJson(body);
  }

  Future<Map<String, dynamic>> replay(String sessionId, {bool refreshHistory = false}) {
    return _http.postJson(
      baseUrl,
      '/api/mobile/workbench/sessions/replay',
      {'sessionId': sessionId, 'refreshHistory': refreshHistory},
    );
  }

  Future<void> pasteImage(String sessionId, String dataUrl) async {
    await _http.postJson(
      baseUrl,
      '/api/mobile/workbench/sessions/paste-image',
      {'sessionId': sessionId, 'dataUrl': dataUrl},
    );
  }

  Future<void> focus(String sessionId) async {
    await _http.postJson(
      baseUrl,
      '/api/mobile/workbench/sessions/focus',
      {'sessionId': sessionId, 'streamActive': true},
    );
  }
}
