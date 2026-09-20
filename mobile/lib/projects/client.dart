import '../core/lan_http.dart';
import '../transfer/client.dart' show transferDeviceId;

/// 对端浏览层 mkdir 能力 token，与后端 `workbench.fs.create-dir.v1` 精确匹配
/// （对齐 web WORKBENCH_FS_CREATE_DIR_CAPABILITY）。
const workbenchFsCreateDirCapability = 'workbench.fs.create-dir.v1';

/// Business Logic: 设备是否经跳板中转（影子设备）决定选择器的「经 X 中转」徽标；
/// 必须与 web isRelayShadowDevice 同口径，避免双端展示不一致。
/// Code Logic: viaDeviceId 为非空字符串即影子设备；字段缺失（直连）返回 false。
bool isRelayShadowDevice(Map<String, dynamic> device) {
  final via = device['viaDeviceId'];
  return via is String && via.isNotEmpty;
}

/// Business Logic: 设备列表在线判定要兼容新旧后端字段——新后端带 status，
/// 旧后端只有 online 布尔（对齐 web decodeMobileTransferDevice 的推导口径）。
/// Code Logic: status 非空优先；否则 online == false 视为 offline，缺省视为 online。
String transferDeviceStatus(Map<String, dynamic> device) {
  final status = device['status'];
  if (status is String && status.isNotEmpty) {
    return status;
  }
  return device['online'] == false ? 'offline' : 'online';
}

/// Business Logic: 影子条目与直连条目可能因探测节奏短暂并存；不去重会让同一台
/// 设备在选择器出现两次且行为不一致（对齐 web dedupeRelayShadowDevices）。
/// Code Logic: 先收集直连 id 集合，再丢弃 id 已有直连条目的影子条目；
/// 输出「直连在前、影子在后」稳定排序，保持同组内原始顺序。
List<Map<String, dynamic>> dedupeRelayShadowDevices(
  List<Map<String, dynamic>> devices,
) {
  final directIds = <String>{
    for (final device in devices)
      if (!isRelayShadowDevice(device)) transferDeviceId(device),
  };
  final direct = <Map<String, dynamic>>[];
  final shadows = <Map<String, dynamic>>[];
  for (final device in devices) {
    if (isRelayShadowDevice(device)) {
      if (!directIds.contains(transferDeviceId(device))) {
        shadows.add(device);
      }
      continue;
    }
    direct.add(device);
  }
  return [...direct, ...shadows];
}

/// Business Logic: 局域网添加入口必须排除主机自己和离线设备，避免把本机当对端
/// 或打开必然失败的离线目标（对齐 web filterOnlineLanDevices）。
/// Code Logic: 先按 relay 规则去重，再仅保留在线且非自身（isSelf != true）的设备。
List<Map<String, dynamic>> filterOnlineLanDevices(
  List<Map<String, dynamic>> devices,
) {
  final deduped = dedupeRelayShadowDevices(devices);
  return deduped
      .where((device) =>
          transferDeviceStatus(device) == 'online' && device['isSelf'] != true)
      .toList(growable: false);
}

/// Business Logic: 旧对端没有浏览层 mkdir；缺能力时必须隐藏「新建文件夹」，
/// 不得回落其它 create-dir 链路（对齐 web peerSupportsBrowseMkdir）。
/// Code Logic: capabilities 为字符串数组且精确包含 mkdir 能力 token 才为 true；
/// 设备条目缺失（未选中）按不支持处理。
bool deviceSupportsBrowseMkdir(Map<String, dynamic>? device) {
  if (device == null) {
    return false;
  }
  final caps = device['capabilities'];
  return caps is List && caps.contains(workbenchFsCreateDirCapability);
}

/// 目录/文件信息（`fs/info` 与 `remote/info` 共同形状，camelCase）。
class ProjectPathInfo {
  const ProjectPathInfo({
    required this.path,
    required this.kind,
    required this.readable,
    this.name = '',
    this.isGitRepo = false,
  });

  final String name;
  final String path;

  /// 'dir' | 'file' | 其它后端扩展值。
  final String kind;

  /// 当前凭据视角是否可读；打开预检必须为 true。
  final bool readable;
  final bool isGitRepo;

  /// Business Logic: info 解析必须宽容，缺字段不能让选择器崩溃。
  /// Code Logic: 宽容读取 name/path/kind/readable/isGitRepo，缺省给安全默认值。
  factory ProjectPathInfo.fromJson(Map<String, dynamic> json) => ProjectPathInfo(
        name: json['name'] as String? ?? '',
        path: json['path'] as String? ?? '',
        kind: json['kind'] as String? ?? 'file',
        readable: json['readable'] == true,
        isGitRepo: json['isGitRepo'] == true,
      );
}

class ProjectSummary {
  const ProjectSummary({
    required this.id,
    required this.name,
    this.kind,
    this.path,
    this.deviceName,
  });

  final String id;
  final String name;
  final String? kind;
  final String? path;

  /// 项目所在设备名（后端 WorkbenchProjectDto.deviceName，camelCase）；
  /// 旧后端缺字段时为 null，UI 侧不展示。
  final String? deviceName;

  /// Business Logic: 项目列表行需要 kind 徽章与设备名对齐 web MobileProjectPanel；
  /// 解析必须宽容，缺字段不能让列表崩溃。
  /// Code Logic: 宽容读取 id/name(title 兜底)/kind/path/deviceName，缺省为空串或 null。
  factory ProjectSummary.fromJson(Map<String, dynamic> json) => ProjectSummary(
        id: json['id'] as String? ?? '',
        name: json['name'] as String? ?? json['title'] as String? ?? '',
        kind: json['kind'] as String?,
        path: json['path'] as String?,
        deviceName: json['deviceName'] as String?,
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

  /// Business Logic: 本机项目打开前必须确认选中路径仍是可读目录，避免 stale
  /// 请求打开旧路径或不可读文件（对齐 web workbenchHttp.fs.info）。
  /// Code Logic: POST `/api/mobile/workbench/fs/info` body `{path}`，
  /// 解析 ProjectPathInfo；请求失败向上抛出由调用方按不可读处理。
  Future<ProjectPathInfo> localPathInfo(String path) async {
    final body = await _http.postDynamic(
      baseUrl,
      '/api/mobile/workbench/fs/info',
      {'path': path},
    );
    return ProjectPathInfo.fromJson(asObject(body));
  }

  /// Business Logic: 远端项目打开前同样必须预检选中远端路径（对齐 web
  /// workbenchHttp.remote.info）。
  /// Code Logic: POST `/api/mobile/workbench/remote/info` body `{deviceId, path}`，
  /// 解析 ProjectPathInfo；请求失败向上抛出由调用方按不可读处理。
  Future<ProjectPathInfo> remotePathInfo({
    required String deviceId,
    required String path,
  }) async {
    final body = await _http.postDynamic(
      baseUrl,
      '/api/mobile/workbench/remote/info',
      {'deviceId': deviceId, 'path': path},
    );
    return ProjectPathInfo.fromJson(asObject(body));
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
