import '../core/lan_http.dart';

class AutomationClient {
  AutomationClient(this._http, this.baseUrl);

  final LanHttpClient _http;
  final String baseUrl;

  Future<List<Map<String, dynamic>>> listTasks(String projectId) async {
    final body = await _http.postDynamic(
      baseUrl,
      '/api/orchestrator/tasks/list',
      {'projectId': projectId},
    );
    return asObjectList(body, wrapKey: 'tasks');
  }

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
      '/api/orchestrator/tasks/create',
      {
        'projectId': projectId,
        'title': title,
        'goal': goal,
        'acceptanceCriteria': acceptanceCriteria,
        'clientRequestId': clientRequestId,
        'createAction': createAction,
      },
    );
  }

  Future<Map<String, dynamic>> taskDetail(String projectId, String taskId) async {
    final tasks = await listTasks(projectId);
    return tasks.firstWhere(
      (task) => task['id'] == taskId,
      orElse: () => {'id': taskId, 'projectId': projectId},
    );
  }

  Future<List<Map<String, dynamic>>> listOutbox(String projectId) async {
    final body = await _http.postDynamic(
      baseUrl,
      '/api/orchestrator/task-views/list',
      {'projectId': projectId},
    );
    return asObjectList(body, wrapKey: 'views')
        .where((view) {
          final kind = view['kind'] as String? ?? '';
          return kind == 'pendingRemote' ||
              kind == 'outbox' ||
              view['status'] == 'failed';
        })
        .toList();
  }

  Future<List<Map<String, dynamic>>> listExperiments(String projectId) async {
    final body = await _http.postDynamic(
      baseUrl,
      '/api/orchestrator/experiments/list',
      {'projectId': projectId},
    );
    return asObjectList(body, wrapKey: 'experiments');
  }

  Future<Map<String, dynamic>> runtimeSnapshot(String projectId) {
    return _http.postJson(
      baseUrl,
      '/api/mobile/orchestrator/runtime-snapshot',
      {'projectId': projectId},
    );
  }
}
