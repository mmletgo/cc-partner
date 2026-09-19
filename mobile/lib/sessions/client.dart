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

  /// 展示名：后端未命名时回退到 id。
  String get displayName => name.isEmpty ? id : name;
}

/// 业务逻辑：进入终端页时应优先恢复指定会话，其次选一个仍在运行的会话，而不是盲选第一个。
///
/// Code Logic：优先返回 id 匹配 preferredId 的会话，否则返回首个非 exited 会话；都没有则返回 null，
/// 由调用方决定新建。
SessionSummary? pickPreferredSession(List<SessionSummary> sessions,
    {String? preferredId}) {
  if (preferredId != null && preferredId.isNotEmpty) {
    for (final session in sessions) {
      if (session.id == preferredId) {
        return session;
      }
    }
  }
  for (final session in sessions) {
    if (session.status != 'exited') {
      return session;
    }
  }
  return null;
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
