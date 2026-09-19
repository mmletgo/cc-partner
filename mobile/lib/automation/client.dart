import 'dart:convert';

import '../core/lan_http.dart';

/// Orchestrator 泳道固定顺序（对齐 web MOBILE_AUTOMATION_WORKFLOW_STATES）。
const List<String> kAutomationWorkflowStates = [
  'backlog',
  'todo',
  'inProgress',
  'humanReview',
  'rework',
  'merging',
  'done',
  'canceled',
];

/// 任务块最少成员数（对齐 web MIN_ORCHESTRATOR_BLOCK_MEMBERS）。
const int kAutomationBlockMinMembers = 2;

/// 任务块最多成员数（对齐 web MAX_ORCHESTRATOR_BLOCK_MEMBERS）。
const int kAutomationBlockMaxMembers = 8;

/// 块末尾追加允许的 head 泳道（对齐 web ORCHESTRATOR_BLOCK_APPEND_STATES）。
const List<String> kAutomationBlockAppendStates = [
  'backlog',
  'todo',
  'inProgress',
];

/// 与 Rust `CAPABILITY_ORCHESTRATOR_TASK_BLOCKS_V1` 对齐的能力 token。
const String kOrchestratorTaskBlocksCapability = 'orchestrator.task-blocks.v1';

/// 从任务 DTO 读取任务块 id；缺失或空白返回 null。
String? automationTaskBlockId(Map<String, dynamic> task) {
  final value = task['blockId'];
  if (value is String && value.trim().isNotEmpty) return value.trim();
  return null;
}

/// 渲染层任务：保留 local/remote 来源与设备元信息的展平任务。
///
/// Business Logic: 泳道列表需要同时展示任务本体与来源设备，块成员也要复用同一渲染行。
/// Code Logic: 从 tagged-union view 中取 task DTO 与 origin/device 字段缓存为不可变对象。
class AutomationRenderableTask {
  AutomationRenderableTask({
    required this.origin,
    required this.task,
    this.deviceId,
    this.deviceName,
    required this.view,
  });

  /// view 来源：local / remote（pendingRemote 不进入该结构）。
  final String origin;

  /// 后端任务 DTO（camelCase，宽容读取）。
  final Map<String, dynamic> task;

  /// remote 来源时的 owning 设备 id。
  final String? deviceId;

  /// remote 来源时的 owning 设备名。
  final String? deviceName;

  /// 原始 task view，供选中详情与 upsert 复用。
  final Map<String, dynamic> view;

  /// 任务 id 兜底空串。
  String get id => task['id'] as String? ?? '';
}

/// 泳道卡片：独立任务或按 blockId 聚合的任务块。
///
/// Business Logic: 块成员不得以独立卡片重复出现在其它泳道，必须整块落在 head 泳道。
/// Code Logic: task 卡片包装单个渲染项；block 卡片携带按 blockIndex 排序的成员与块标题。
class AutomationBoardItem {
  AutomationBoardItem.task(AutomationRenderableTask this.item)
      : isBlock = false,
        blockId = null,
        title = null,
        members = const [];

  AutomationBoardItem.block({
    required this.blockId,
    required this.title,
    required this.members,
  })  : isBlock = true,
        item = null;

  /// 是否任务块卡片。
  final bool isBlock;

  /// task 卡片时的渲染任务；block 卡片为 null。
  final AutomationRenderableTask? item;

  /// block 卡片时的块 id。
  final String? blockId;

  /// block 卡片标题：首个非空 blockTitle → 首个成员标题 → blockId。
  final String? title;

  /// block 卡片成员（已按 blockIndex 排序）。
  final List<AutomationRenderableTask> members;
}

/// 按块内展示顺序排序成员：blockIndex 升序，缺失视为 0，再用 createdAt/id 打破平局。
List<AutomationRenderableTask> automationSortBlockMembers(
  List<AutomationRenderableTask> members,
) {
  int blockIndex(AutomationRenderableTask member) {
    final value = member.task['blockIndex'];
    return value is num ? value.toInt() : 0;
  }

  final sorted = [...members];
  sorted.sort((left, right) {
    final leftIndex = blockIndex(left);
    final rightIndex = blockIndex(right);
    if (leftIndex != rightIndex) return leftIndex.compareTo(rightIndex);
    final created = (left.task['createdAt'] as String? ?? '')
        .compareTo(right.task['createdAt'] as String? ?? '');
    if (created != 0) return created;
    return left.id.compareTo(right.id);
  });
  return sorted;
}

/// 块卡片所在泳道：第一个未完成成员所在泳道；全部终态则进 done。
String automationBlockHeadLane(List<AutomationRenderableTask> sortedMembers) {
  for (final member in sortedMembers) {
    final state = member.task['workflowState'] as String? ?? 'backlog';
    if (state != 'done' && state != 'canceled') return state;
  }
  return 'done';
}

/// 把 task views 聚合成泳道 → 卡片列表（对齐 web groupBoardItems）。
///
/// Business Logic: 移动端列表是桌面 workflow board 的 compact 视图，块必须聚合为一块卡片。
/// Code Logic: 无 blockId 的任务按自身 workflowState 入列；有 blockId 的成员排序后整块放入 head 泳道。
Map<String, List<AutomationBoardItem>> automationGroupBoardItems(
  List<Map<String, dynamic>> views,
) {
  final groups = <String, List<AutomationBoardItem>>{
    for (final lane in kAutomationWorkflowStates) lane: <AutomationBoardItem>[],
  };
  final blocks = <String, List<AutomationRenderableTask>>{};
  for (final view in views) {
    final task = automationTaskOfView(view);
    if (task == null) continue;
    final renderable = AutomationRenderableTask(
      origin: view['origin'] as String? ?? 'local',
      task: task,
      deviceId: view['deviceId'] as String?,
      deviceName: view['deviceName'] as String?,
      view: view,
    );
    final blockId = automationTaskBlockId(task);
    if (blockId == null) {
      groups
          .putIfAbsent(
            task['workflowState'] as String? ?? 'backlog',
            () => <AutomationBoardItem>[],
          )
          .add(AutomationBoardItem.task(renderable));
      continue;
    }
    blocks.putIfAbsent(blockId, () => <AutomationRenderableTask>[]).add(renderable);
  }
  for (final entry in blocks.entries) {
    final sorted = automationSortBlockMembers(entry.value);
    final titled = sorted.where(
      (member) =>
          (member.task['blockTitle'] as String? ?? '').trim().isNotEmpty,
    );
    final title = titled.isNotEmpty
        ? (titled.first.task['blockTitle'] as String).trim()
        : sorted.isNotEmpty
            ? sorted.first.task['title'] as String? ?? entry.key
            : entry.key;
    groups
        .putIfAbsent(
          automationBlockHeadLane(sorted),
          () => <AutomationBoardItem>[],
        )
        .add(
          AutomationBoardItem.block(
            blockId: entry.key,
            title: title,
            members: sorted,
          ),
        );
  }
  return groups;
}

/// 重排只允许整块尚未开工：>=2 个成员且全部 backlog/todo + runState=idle。
bool automationCanReorderBlock(List<AutomationRenderableTask> members) {
  if (members.length < kAutomationBlockMinMembers) return false;
  return members.every((member) {
    final workflow = member.task['workflowState'] as String? ?? '';
    return (workflow == 'backlog' || workflow == 'todo') &&
        (member.task['runState'] as String? ?? '') == 'idle';
  });
}

/// 追加只允许 head 仍在 backlog/todo/inProgress，且无人进入复核/返工/合入/交付。
bool automationCanAppendToBlock(List<AutomationRenderableTask> members) {
  if (members.length >= kAutomationBlockMaxMembers) return false;
  final head = automationBlockHeadLane(automationSortBlockMembers(members));
  if (!kAutomationBlockAppendStates.contains(head)) return false;
  return members.every((member) {
    final workflow = member.task['workflowState'] as String? ?? '';
    if (workflow == 'humanReview' ||
        workflow == 'rework' ||
        workflow == 'merging') {
      return false;
    }
    return (member.task['runState'] as String? ?? '') != 'delivering';
  });
}

/// 对端协议能力是否支持任务块（复刻 PeerProtocolInfo::supports）。
///
/// Business Logic: 旧 peer / v0 health 不得被当成支持任务块，否则 UI 会放出后端必失败的按钮。
/// Code Logic: version(protocol_version/protoVersion)>=1 且 capabilities 精确包含 token。
bool automationPeerSupportsTaskBlocks(Map<String, dynamic>? peer) {
  if (peer == null) return false;
  final version = peer['protocol_version'] ?? peer['protoVersion'];
  final parsed = version is num ? version.toInt() : 0;
  if (parsed < 1) return false;
  final capabilities = peer['capabilities'];
  return capabilities is List &&
      capabilities.contains(kOrchestratorTaskBlocksCapability);
}

/// 任务块创建能力门控（对齐 web canCreateOrchestratorTaskBlock）。
///
/// Business Logic: 本机项目与当前 UI 同版本始终可建块；remote shortcut 必须看 owner 能力。
/// Code Logic: 无 kind → false（fail-closed）；非 remote → true；remote → peer 支持（缺 peer 拒绝）。
bool automationCanCreateTaskBlock({
  String? projectKind,
  Map<String, dynamic>? peer,
}) {
  if (projectKind == null) return false;
  if (projectKind != 'remote') return true;
  return automationPeerSupportsTaskBlocks(peer);
}

/// task view 的稳定 upsert key：pendingRemote 用 item.id，其余用 task.id。
String automationViewStableKey(Map<String, dynamic> view) {
  if (view['origin'] == 'pendingRemote') {
    final item = view['item'];
    final id = item is Map ? item['id'] as String? ?? '' : '';
    return 'pending:$id';
  }
  return 'task:${automationTaskOfView(view)?['id'] ?? ''}';
}

/// 创建/重排/追加返回单个 view 时稳定合并进当前列表；找不到时插到列表头部。
List<Map<String, dynamic>> automationUpsertView(
  List<Map<String, dynamic>> current,
  Map<String, dynamic> next,
) {
  final key = automationViewStableKey(next);
  final index = current.indexWhere((view) => automationViewStableKey(view) == key);
  if (index == -1) return [next, ...current];
  return [
    for (var i = 0; i < current.length; i++)
      i == index ? Map<String, dynamic>.from(next) : current[i],
  ];
}

/// 创建任务块后把全部成员视图合并进当前列表；裸任务 DTO 回落成本地 view。
List<Map<String, dynamic>> automationUpsertBlockCreated(
  List<Map<String, dynamic>> current,
  Map<String, dynamic> created,
) {
  var views = current;
  for (final item in asObjectList(created['tasks'])) {
    final view = item.containsKey('origin')
        ? item
        : <String, dynamic>{'origin': 'local', 'task': item};
    views = automationUpsertView(views, view);
  }
  return views;
}

/// 实验组是否等待人工决策（对齐 web needsDecision || winnerReady）。
bool automationExperimentNeedsDecision(Map<String, dynamic> experiment) {
  final status = experiment['status'] as String? ?? '';
  return status == 'needsDecision' || status == 'winnerReady';
}

/// 实验组推荐 winner：winnerTaskId 优先，否则首个 ready candidate 的 taskId。
String? automationExperimentRecommendedTaskId(Map<String, dynamic> experiment) {
  final winner = experiment['winnerTaskId'];
  if (winner is String && winner.isNotEmpty) return winner;
  for (final candidate in asObjectList(experiment['candidates'])) {
    final outcome = candidate['outcome'] as String? ?? '';
    if (outcome == 'candidateReady' || outcome == 'winner') {
      final taskId = candidate['taskId'];
      if (taskId is String && taskId.isNotEmpty) return taskId;
    }
  }
  return null;
}

/// 任务视图拆分结果：真实任务（local/remote）与待发送 outbox 条目。
///
/// Business Logic: 手机端要把远端离线待发送项与真实任务分开渲染，只有真实任务可展开详情。
/// Code Logic: 单次遍历 task views，pendingRemote 收集 item，其余保留视图原样。
class AutomationViewSplit {
  AutomationViewSplit({required this.tasks, required this.pendingRemoteItems});

  /// origin 为 local/remote 的任务视图（原样保留，用 [automationTaskOfView] 取任务）。
  final List<Map<String, dynamic>> tasks;

  /// pendingRemote 视图中的 outbox item（id/status/deviceName/lastError/requestJson）。
  final List<Map<String, dynamic>> pendingRemoteItems;
}

/// 从任务视图提取任务 DTO；pendingRemote 或缺 task 字段时返回 null。
Map<String, dynamic>? automationTaskOfView(Map<String, dynamic> view) {
  if (view['origin'] == 'pendingRemote') return null;
  final task = view['task'];
  if (task is Map<String, dynamic>) return task;
  if (task is Map) return Map<String, dynamic>.from(task);
  return null;
}

/// 拆分后端 tagged-union 任务视图（对齐 web splitOrchestratorTaskViews）。
AutomationViewSplit automationSplitViews(List<Map<String, dynamic>> views) {
  final tasks = <Map<String, dynamic>>[];
  final pending = <Map<String, dynamic>>[];
  for (final view in views) {
    if (view['origin'] == 'pendingRemote') {
      final item = view['item'];
      if (item is Map) {
        pending.add(Map<String, dynamic>.from(item));
      }
      continue;
    }
    tasks.add(view);
  }
  return AutomationViewSplit(tasks: tasks, pendingRemoteItems: pending);
}

/// 按固定泳道分组真实任务视图（key 为 workflowState，缺省落 backlog）。
Map<String, List<Map<String, dynamic>>> automationGroupByWorkflow(
  List<Map<String, dynamic>> views,
) {
  final groups = <String, List<Map<String, dynamic>>>{
    for (final lane in kAutomationWorkflowStates) lane: <Map<String, dynamic>>[],
  };
  for (final view in views) {
    final task = automationTaskOfView(view);
    if (task == null) continue;
    final lane = task['workflowState'] as String? ?? 'backlog';
    groups.putIfAbsent(lane, () => <Map<String, dynamic>>[]).add(view);
  }
  return groups;
}

/// 从 outbox requestJson 解析用户填写标题；失败时用远端项目路径兜底。
///
/// Business Logic: 远端离线创建落 outbox 时仍应展示用户填写的任务标题，
/// 而不是只看到设备路径（对齐 web pendingRemoteTaskTitle）。
String automationOutboxTitle(Map<String, dynamic> item) {
  final requestJson = item['requestJson'];
  if (requestJson is String && requestJson.isNotEmpty) {
    try {
      final parsed = jsonDecode(requestJson);
      if (parsed is Map) {
        final title = parsed['title'];
        if (title is String && title.trim().isNotEmpty) return title.trim();
      }
    } catch (_) {
      // requestJson 来自本机 outbox，解析失败时走路径兜底即可。
    }
  }
  return item['remoteProjectPath'] as String? ?? item['id'] as String? ?? '';
}

/// Orchestrator 移动端 HTTP 客户端（对齐 web workbenchHttp orchestrator 段路由与 body）。
class AutomationClient {
  AutomationClient(this._http, this.baseUrl);

  final LanHttpClient _http;
  final String baseUrl;

  /// 泳道任务视图：POST /api/orchestrator/task-views/list，返回 {origin, task|item} 视图列表。
  Future<List<Map<String, dynamic>>> listViews(String projectId) async {
    final body = await _http.postDynamic(
      baseUrl,
      '/api/orchestrator/task-views/list',
      {'projectId': projectId},
    );
    return asObjectList(body, wrapKey: 'views');
  }

  /// 旧平铺任务列表（兼容保留）：POST /api/orchestrator/tasks/list。
  Future<List<Map<String, dynamic>>> listTasks(String projectId) async {
    final body = await _http.postDynamic(
      baseUrl,
      '/api/orchestrator/tasks/list',
      {'projectId': projectId},
    );
    return asObjectList(body, wrapKey: 'tasks');
  }

  /// 创建任务并返回任务视图：POST /api/orchestrator/task-views/create。
  ///
  /// [createAction] 为 backlog/todo/start；[clientRequestId] 用于 owner 侧幂等去重。
  Future<Map<String, dynamic>> createTask({
    required String projectId,
    required String title,
    required String goal,
    required String acceptanceCriteria,
    required String clientRequestId,
    String createAction = 'backlog',
  }) {
    return _http.postJson(
      baseUrl,
      '/api/orchestrator/task-views/create',
      {
        'projectId': projectId,
        'title': title,
        'goal': goal,
        'acceptanceCriteria': acceptanceCriteria,
        'priority': 0,
        'createAction': createAction,
        'clientRequestId': clientRequestId,
      },
    );
  }

  /// AI 完善 Prompt：POST /api/orchestrator/tasks/complete-prompt。
  ///
  /// 返回 {title, goal, acceptanceCriteria}；本机项目可传 workingDirectory 提升效果。
  Future<Map<String, dynamic>> completePrompt(
    String projectId,
    String prompt, {
    String? workingDirectory,
  }) {
    return _http.postJson(
      baseUrl,
      '/api/orchestrator/tasks/complete-prompt',
      {
        'projectId': projectId,
        'prompt': prompt,
        'workingDirectory': workingDirectory,
      },
    );
  }

  /// 任务证据时间线：POST /api/orchestrator/tasks/evidence。
  Future<List<Map<String, dynamic>>> listEvidence(
    String projectId,
    String taskId,
  ) async {
    final body = await _http.postDynamic(
      baseUrl,
      '/api/orchestrator/tasks/evidence',
      {'projectId': projectId, 'taskId': taskId},
    );
    return asObjectList(body, wrapKey: 'evidence');
  }

  /// 重试 failed outbox 条目：POST /api/orchestrator/outbox/retry。
  Future<Map<String, dynamic>> retryOutbox(String projectId, String outboxId) {
    return _http.postJson(baseUrl, '/api/orchestrator/outbox/retry', {
      'projectId': projectId,
      'outboxId': outboxId,
    });
  }

  /// 丢弃 failed outbox 条目（进入 discarded 审计终态）：POST /api/orchestrator/outbox/discard。
  Future<Map<String, dynamic>> discardOutbox(String projectId, String outboxId) {
    return _http.postJson(baseUrl, '/api/orchestrator/outbox/discard', {
      'projectId': projectId,
      'outboxId': outboxId,
    });
  }

  /// 离线待同步条目：基于 task-views/list 的 pendingRemote 视图。
  Future<List<Map<String, dynamic>>> listOutbox(String projectId) async {
    final views = await listViews(projectId);
    return automationSplitViews(views).pendingRemoteItems;
  }

  /// 任务详情（兼容保留）：从平铺任务列表里按 id 匹配。
  Future<Map<String, dynamic>> taskDetail(String projectId, String taskId) async {
    final tasks = await listTasks(projectId);
    return tasks.firstWhere(
      (task) => task['id'] == taskId,
      orElse: () => {'id': taskId, 'projectId': projectId},
    );
  }

  /// 实验组列表：POST /api/orchestrator/experiments/list。
  Future<List<Map<String, dynamic>>> listExperiments(String projectId) async {
    final body = await _http.postDynamic(
      baseUrl,
      '/api/orchestrator/experiments/list',
      {'projectId': projectId},
    );
    return asObjectList(body, wrapKey: 'experiments');
  }

  /// 本机/远端 runtime 快照：POST /api/mobile/orchestrator/runtime-snapshot。
  Future<Map<String, dynamic>> runtimeSnapshot(String projectId) {
    return _http.postJson(
      baseUrl,
      '/api/mobile/orchestrator/runtime-snapshot',
      {'projectId': projectId},
    );
  }

  /// 创建任务块：POST /api/orchestrator/task-views/create-block。
  ///
  /// [members] 每个成员只提交 title/goal/acceptanceCriteria 三字段；
  /// [clientRequestId] 与单任务创建同策略（逻辑提交周期内复用）。
  Future<Map<String, dynamic>> createBlock({
    required String projectId,
    required String title,
    required List<Map<String, String>> members,
    required String createAction,
    required String clientRequestId,
  }) {
    return _http.postJson(
      baseUrl,
      '/api/orchestrator/task-views/create-block',
      {
        'projectId': projectId,
        'title': title,
        'members': members,
        'createAction': createAction,
        'clientRequestId': clientRequestId,
      },
    );
  }

  /// 块末尾追加成员：POST /api/orchestrator/task-views/append-block-member。
  ///
  /// 只提交 title/goal/acceptanceCriteria 三字段，返回新成员的 task view。
  Future<Map<String, dynamic>> appendBlockMember({
    required String projectId,
    required String blockId,
    required String title,
    required String goal,
    required String acceptanceCriteria,
    required String clientRequestId,
  }) {
    return _http.postJson(
      baseUrl,
      '/api/orchestrator/task-views/append-block-member',
      {
        'projectId': projectId,
        'blockId': blockId,
        'title': title,
        'goal': goal,
        'acceptanceCriteria': acceptanceCriteria,
        'clientRequestId': clientRequestId,
      },
    );
  }

  /// 块成员重排：POST /api/orchestrator/task-views/reorder-block-members。
  ///
  /// [orderedTaskIds] 为交换相邻成员后的完整置换；返回全部成员的最新 task views。
  Future<List<Map<String, dynamic>>> reorderBlockMembers({
    required String projectId,
    required String blockId,
    required List<String> orderedTaskIds,
    required String clientRequestId,
  }) async {
    final body = await _http.postDynamic(
      baseUrl,
      '/api/orchestrator/task-views/reorder-block-members',
      {
        'projectId': projectId,
        'blockId': blockId,
        'orderedTaskIds': orderedTaskIds,
        'clientRequestId': clientRequestId,
      },
    );
    return asObjectList(body);
  }

  /// 采纳实验组推荐 winner：POST /api/orchestrator/experiments/approve-winner。
  Future<Map<String, dynamic>> approveExperimentWinner(
    String experimentId,
    String winnerTaskId,
  ) {
    return _http.postJson(
      baseUrl,
      '/api/orchestrator/experiments/approve-winner',
      {
        'experimentId': experimentId,
        'winnerTaskId': winnerTaskId,
        'reason': null,
      },
    );
  }

  /// 取消整组实验：POST /api/orchestrator/experiments/cancel。
  Future<Map<String, dynamic>> cancelExperiment(String experimentId) {
    return _http.postJson(
      baseUrl,
      '/api/orchestrator/experiments/cancel',
      {'experimentId': experimentId},
    );
  }

  /// 局域网设备清单（含 capabilities/protoVersion）：GET /api/mobile/devices。
  Future<List<Map<String, dynamic>>> listDevices() async {
    final body = await _http.getDynamic(baseUrl, '/api/mobile/devices');
    return asObjectList(body, wrapKey: 'devices');
  }

  /// 读取项目 owner 设备 id：从 projects/list 原始 DTO 提取 deviceId。
  ///
  /// Business Logic: remote 项目的任务块能力要看 owner 设备协议能力，需要 projectId → deviceId 映射。
  /// Code Logic: GET projects/list 后按项目 id 匹配并读取 deviceId 字段；找不到返回 null。
  Future<String?> projectOwnerDeviceId(String projectId) async {
    final body = await _http.getDynamic(
      baseUrl,
      '/api/mobile/workbench/projects/list',
    );
    for (final project in asObjectList(body, wrapKey: 'projects')) {
      if (project['id'] == projectId) {
        final deviceId = project['deviceId'];
        if (deviceId is String && deviceId.isNotEmpty) return deviceId;
        return null;
      }
    }
    return null;
  }
}
