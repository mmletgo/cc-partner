import '../core/lan_http.dart';

class ProjectSummary {
  const ProjectSummary({
    required this.id,
    required this.name,
    this.kind,
    this.path,
  });

  final String id;
  final String name;
  final String? kind;
  final String? path;

  factory ProjectSummary.fromJson(Map<String, dynamic> json) => ProjectSummary(
        id: json['id'] as String? ?? '',
        name: json['name'] as String? ?? json['title'] as String? ?? '',
        kind: json['kind'] as String?,
        path: json['path'] as String?,
      );
}

/// Lists / opens / removes Workbench projects on the current PC.
class ProjectsClient {
  ProjectsClient(this._http, this.baseUrl);

  final LanHttpClient _http;
  final String baseUrl;

  Future<List<ProjectSummary>> listRecent() async {
    final body = await _http.getDynamic(
      baseUrl,
      '/api/mobile/workbench/projects/list',
    );
    return asObjectList(body, wrapKey: 'projects')
        .map(ProjectSummary.fromJson)
        .toList();
  }

  Future<ProjectSummary> open({
    required String path,
    String? deviceId,
  }) async {
    final body = await _http.postJson(
      baseUrl,
      '/api/mobile/workbench/projects/open',
      {
        'path': path,
        if (deviceId != null) 'deviceId': deviceId,
      },
    );
    final project = body['project'] as Map<String, dynamic>? ?? body;
    return ProjectSummary.fromJson(project);
  }

  Future<void> remove(String projectId) async {
    await _http.postJson(
      baseUrl,
      '/api/mobile/workbench/projects/remove',
      {'projectId': projectId},
    );
  }

  Future<List<Map<String, dynamic>>> listLocalRoots() async {
    final body = await _http.getDynamic(baseUrl, '/api/mobile/workbench/fs/roots');
    return asObjectList(body, wrapKey: 'roots');
  }

  Future<List<Map<String, dynamic>>> listLocalDir(String path) async {
    final body = await _http.postDynamic(
      baseUrl,
      '/api/mobile/workbench/fs/list',
      {'path': path},
    );
    return asObjectList(body, wrapKey: 'entries');
  }

  Future<Map<String, dynamic>> createDir({
    required String parentPath,
    required String name,
  }) {
    return _http.postJson(
      baseUrl,
      '/api/mobile/workbench/fs/create-dir',
      {'parentPath': parentPath, 'name': name},
    );
  }

  Future<List<Map<String, dynamic>>> listRemoteRoots(String deviceId) async {
    final body = await _http.postDynamic(
      baseUrl,
      '/api/mobile/workbench/remote/roots',
      {'deviceId': deviceId},
    );
    return asObjectList(body, wrapKey: 'roots');
  }

  Future<List<Map<String, dynamic>>> listRemoteDir({
    required String deviceId,
    required String path,
  }) async {
    final body = await _http.postDynamic(
      baseUrl,
      '/api/mobile/workbench/remote/list',
      {'deviceId': deviceId, 'path': path},
    );
    return asObjectList(body, wrapKey: 'entries');
  }

  Future<Map<String, dynamic>> createRemoteDir({
    required String deviceId,
    required String parentPath,
    required String name,
  }) {
    return _http.postJson(
      baseUrl,
      '/api/mobile/workbench/remote/create-dir',
      {'deviceId': deviceId, 'parentPath': parentPath, 'name': name},
    );
  }

  Future<ProjectSummary> openRemote({
    required String deviceId,
    required String path,
  }) async {
    final body = await _http.postJson(
      baseUrl,
      '/api/mobile/workbench/remote/open',
      {'deviceId': deviceId, 'path': path},
    );
    final project = body['project'] as Map<String, dynamic>? ?? body;
    return ProjectSummary.fromJson(project);
  }
}
