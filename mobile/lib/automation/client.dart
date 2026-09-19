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
}
