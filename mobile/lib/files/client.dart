import '../core/lan_http.dart';

class FilesClient {
  FilesClient(this._http, this.baseUrl);

  final LanHttpClient _http;
  final String baseUrl;

  Future<List<Map<String, dynamic>>> listDir({
    required String projectId,
    String? worktreeId,
    String? path,
  }) async {
    final body = await _http.postDynamic(
      baseUrl,
      '/api/mobile/workbench/files/list-dir',
      {
        'projectId': projectId,
        'worktreeId': worktreeId,
        'path': path,
      },
    );
    return asObjectList(body);
  }

  Future<Map<String, dynamic>> previewHtmlAsset({
    required String projectId,
    required String documentPath,
    required String assetPath,
    String? worktreeId,
  }) {
    return _http.postJson(
      baseUrl,
      '/api/workbench/files/preview-html-asset',
      {
        'projectId': projectId,
        'worktreeId': worktreeId,
        'documentPath': documentPath,
        'assetPath': assetPath,
      },
    );
  }

  Future<Map<String, dynamic>> open({
    required String projectId,
    required String path,
    String? worktreeId,
  }) {
    return _http.postJson(
      baseUrl,
      '/api/mobile/workbench/files/open',
      {
        'projectId': projectId,
        'worktreeId': worktreeId,
        'path': path,
      },
    );
  }

  Future<Map<String, dynamic>> previewSqlite({
    required String projectId,
    required String path,
    String? worktreeId,
    String? table,
    int limitRows = 200,
  }) {
    return _http.postJson(
      baseUrl,
      '/api/workbench/files/preview-sqlite',
      {
        'projectId': projectId,
        'worktreeId': worktreeId,
        'path': path,
        'table': table,
        'limitRows': limitRows,
      },
    );
  }

  Future<Map<String, dynamic>> saveText({
    required String projectId,
    required String path,
    required String content,
    required String baseHash,
    String? worktreeId,
  }) {
    return _http.postJson(
      baseUrl,
      '/api/mobile/workbench/files/save-text',
      {
        'projectId': projectId,
        'worktreeId': worktreeId,
        'path': path,
        'content': content,
        'baseHash': baseHash,
      },
    );
  }
}
