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
    final body = await _http.postJson(
      baseUrl,
      '/api/mobile/workbench/projects/list',
      const {},
    );
    final items = body['projects'] as List<dynamic>? ??
        body['items'] as List<dynamic>? ??
        const [];
    return items
        .map((e) => ProjectSummary.fromJson(e as Map<String, dynamic>))
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
}
