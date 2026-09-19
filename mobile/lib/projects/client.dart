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

  /// Business Logic: 项目列表页顶部需要跨设备 Agent Fleet 摘要
  /// （对齐 web useLanAgentFleet 的 getSnapshot）。
  /// Code Logic: POST `/api/mobile/workbench/lan-fleet` 空 body，
  /// 返回 camelCase LanFleetSnapshot（devices[].reachability/projects[].agentCounts）。
  Future<Map<String, dynamic>> fleetSnapshot() {
    return _http.postJson(
      baseUrl,
      '/api/mobile/workbench/lan-fleet',
      const <String, dynamic>{},
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

/// 项目列表顶部的 Agent Fleet 轻量摘要（对齐 web MobileProjectPanel fleetSummary）。
class LanFleetOverview {
  const LanFleetOverview({
    required this.offlineDevices,
    required this.exceptionAgents,
  });

  /// reachability == 'offline' 的设备数。
  final int offlineDevices;

  /// 全部设备全部项目的 needsInput + failed agent 总数（与 web fleetExceptionCount 同口径）。
  final int exceptionAgents;

  /// Business Logic: 项目列表需要一条「N 台设备 · M 需处理 · K 台离线」式摘要，
  /// 数据缺失时整行隐藏，不能阻塞项目列表主流程。
  /// Code Logic: 宽容解析 snapshot.devices → reachability 与 projects[].agentCounts
  /// 的 needsInput/failed（负数按 0 饱和，对齐 web fleetExceptionCount）；
  /// devices 缺失或非列表时返回 null（调用方隐藏摘要行）。
  static LanFleetOverview? fromSnapshot(Map<String, dynamic> snapshot) {
    final devices = snapshot['devices'];
    if (devices is! List) {
      return null;
    }
    var offline = 0;
    var exceptions = 0;
    for (final device in devices) {
      if (device is! Map) {
        continue;
      }
      if (device['reachability'] == 'offline') {
        offline += 1;
      }
      final projects = device['projects'];
      if (projects is! List) {
        continue;
      }
      for (final project in projects) {
        if (project is! Map) {
          continue;
        }
        final counts = project['agentCounts'];
        if (counts is! Map) {
          continue;
        }
        final needsInput = (counts['needsInput'] as num?)?.toInt() ?? 0;
        final failed = (counts['failed'] as num?)?.toInt() ?? 0;
        exceptions +=
            (needsInput > 0 ? needsInput : 0) + (failed > 0 ? failed : 0);
      }
    }
    return LanFleetOverview(offlineDevices: offline, exceptionAgents: exceptions);
  }

  /// Business Logic: 摘要行文案口径照 web fleetSummary（标题 · N 需处理 · 设备离线 (K)，
  /// 0 值段不展示）。
  /// Code Logic: 纯拼接；exceptionAgents/offlineDevices 为 0 时省略对应段。
  String get label {
    final buffer = StringBuffer('局域网 Agent Fleet');
    if (exceptionAgents > 0) {
      buffer.write(' · $exceptionAgents 需处理');
    }
    if (offlineDevices > 0) {
      buffer.write(' · 设备离线 ($offlineDevices)');
    }
    return buffer.toString();
  }
}
