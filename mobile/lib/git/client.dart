import '../core/lan_http.dart';

class GitClient {
  GitClient(this._http, this.baseUrl);

  final LanHttpClient _http;
  final String baseUrl;

  Future<Map<String, dynamic>> listWorktrees(String projectId) {
    return _http.postJson(
      baseUrl,
      '/api/mobile/workbench/worktrees/list',
      {'projectId': projectId, 'includeGitStatus': false},
    );
  }

  Future<Map<String, dynamic>> commit({
    required String projectId,
    required String worktreeId,
    required String clientOperationId,
  }) {
    return _http.postJson(
      baseUrl,
      '/api/mobile/workbench/worktrees/commit',
      {
        'projectId': projectId,
        'worktreeId': worktreeId,
        'clientOperationId': clientOperationId,
      },
    );
  }

  Future<Map<String, dynamic>> pull({
    required String projectId,
    required String worktreeId,
  }) {
    return _http.postJson(
      baseUrl,
      '/api/mobile/workbench/worktrees/pull',
      {'projectId': projectId, 'worktreeId': worktreeId},
    );
  }

  Future<Map<String, dynamic>> push({
    required String projectId,
    required String worktreeId,
  }) {
    return _http.postJson(
      baseUrl,
      '/api/mobile/workbench/worktrees/push',
      {'projectId': projectId, 'worktreeId': worktreeId},
    );
  }

  Future<Map<String, dynamic>> merge({
    required String projectId,
    required String worktreeId,
    required String clientOperationId,
  }) {
    return _http.postJson(
      baseUrl,
      '/api/mobile/workbench/worktrees/merge',
      {
        'projectId': projectId,
        'worktreeId': worktreeId,
        'clientOperationId': clientOperationId,
      },
    );
  }
}
