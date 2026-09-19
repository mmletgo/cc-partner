import '../core/lan_http.dart';

/// worktree 卡片显示名：优先 name，其次 branch / id。
String worktreeDisplayName(Map<String, dynamic> tree) =>
    tree['name'] as String? ?? tree['branch'] as String? ?? tree['id'] as String? ?? '';

/// 工作台 Git 引用标签（本地分支 / 远端分支 / tag），字段对齐 web `WorkbenchGitRef`。
class WorkbenchGitRef {
  const WorkbenchGitRef({
    this.name = '',
    this.fullName = '',
    this.kind = 'other',
    this.remote,
    this.isHead = false,
  });

  /// 宽容解析：缺字段时回退默认值，非对象输入返回空 ref。
  factory WorkbenchGitRef.fromJson(Object? raw) {
    if (raw is! Map) {
      return const WorkbenchGitRef();
    }
    final name = raw['name'] as String? ?? '';
    return WorkbenchGitRef(
      name: name,
      fullName: raw['fullName'] as String? ?? name,
      kind: raw['kind'] as String? ?? 'other',
      remote: raw['remote'] as String?,
      isHead: raw['isHead'] == true,
    );
  }

  final String name;

  /// 完整引用路径，如 refs/heads/main。
  final String fullName;

  /// local | remote | tag | head | other
  final String kind;
  final String? remote;
  final bool isHead;
}

/// 工作台 Git 提交摘要，字段对齐 web `WorkbenchGitCommit`。
class WorkbenchGitCommit {
  const WorkbenchGitCommit({
    this.hash = '',
    this.shortHash = '',
    this.parentHashes = const [],
    this.authorName = '',
    this.authorEmail = '',
    this.authoredAt = '',
    this.summary = '',
    this.refs = const [],
  });

  /// 宽容解析：缺字段时给默认值，保证旧后端/异常数据不炸 UI。
  factory WorkbenchGitCommit.fromJson(Object? raw) {
    if (raw is! Map) {
      return const WorkbenchGitCommit();
    }
    return WorkbenchGitCommit(
      hash: raw['hash'] as String? ?? '',
      shortHash: raw['shortHash'] as String? ?? '',
      parentHashes: (raw['parentHashes'] as List?)?.whereType<String>().toList() ?? const [],
      authorName: raw['authorName'] as String? ?? '',
      authorEmail: raw['authorEmail'] as String? ?? '',
      authoredAt: raw['authoredAt'] as String? ?? '',
      summary: raw['summary'] as String? ?? '',
      refs: (raw['refs'] as List?)?.map(WorkbenchGitRef.fromJson).toList() ?? const [],
    );
  }

  final String hash;
  final String shortHash;
  final List<String> parentHashes;
  final String authorName;
  final String authorEmail;

  /// ISO 8601 字符串。
  final String authoredAt;
  final String summary;
  final List<WorkbenchGitRef> refs;
}

/// worktree 运行期 Git 状态摘要（`listWorktrees(includeGitStatus: true)` 时返回）。
class WorktreeGitStatus {
  const WorktreeGitStatus({
    this.branch,
    this.changed = 0,
    this.ahead = 0,
    this.behind = 0,
    this.conflicts = 0,
    this.clean = true,
    this.canPush = false,
    this.present = false,
  });

  /// 宽容解析：缺字段时回退默认值；非对象输入视为后端未提供状态。
  factory WorktreeGitStatus.fromJson(Object? raw) {
    if (raw is! Map) {
      return const WorktreeGitStatus();
    }
    return WorktreeGitStatus(
      present: true,
      branch: raw['branch'] as String?,
      changed: (raw['changed'] as num?)?.toInt() ?? 0,
      ahead: (raw['ahead'] as num?)?.toInt() ?? 0,
      behind: (raw['behind'] as num?)?.toInt() ?? 0,
      conflicts: (raw['conflicts'] as num?)?.toInt() ?? 0,
      clean: raw['clean'] != false,
      canPush: raw['canPush'] == true,
    );
  }

  /// 从 worktree 节点取 status；缺失时返回 present=false 的默认值。
  factory WorktreeGitStatus.of(Map<String, dynamic> tree) =>
      WorktreeGitStatus.fromJson(tree['status']);

  /// 后端是否真的返回了 status 字段。
  final bool present;
  final String? branch;
  final int changed;
  final int ahead;
  final int behind;
  final int conflicts;
  final bool clean;
  final bool canPush;
}

class GitClient {
  GitClient(this._http, this.baseUrl);

  final LanHttpClient _http;
  final String baseUrl;

  /// 列出项目 worktrees；includeGitStatus 控制是否附带运行期 Git 状态。
  ///
  /// 默认 false 与既有调用方（workbench_home 等）行为一致；Git 面板传 true。
  Future<Map<String, dynamic>> listWorktrees(
    String projectId, {
    bool includeGitStatus = false,
  }) {
    return _http.postJson(
      baseUrl,
      '/api/mobile/workbench/worktrees/list',
      {'projectId': projectId, 'includeGitStatus': includeGitStatus},
    );
  }

  /// 当前 worktree 最近提交，对齐 web `git.listCommits`。
  ///
  /// Code Logic: POST /api/mobile/workbench/git/commits，body {projectId, worktreeId, limit}；
  /// 后端返回裸数组，兼容包一层 `commits` 的形态；条目宽容解析。
  Future<List<WorkbenchGitCommit>> commits(
    String projectId, {
    String? worktreeId,
    int limit = 30,
  }) async {
    final decoded = await _http.postDynamic(
      baseUrl,
      '/api/mobile/workbench/git/commits',
      {'projectId': projectId, 'worktreeId': worktreeId, 'limit': limit},
    );
    List<dynamic> raw;
    if (decoded is List) {
      raw = decoded;
    } else if (decoded is Map && decoded['commits'] is List) {
      raw = decoded['commits'] as List;
    } else {
      raw = const [];
    }
    return raw.map(WorkbenchGitCommit.fromJson).toList();
  }

  Future<Map<String, dynamic>> commit({
    required String worktreeId,
    required String clientOperationId,
    String? message,
  }) {
    return _http.postJson(
      baseUrl,
      '/api/mobile/workbench/worktrees/commit',
      {
        'worktreeId': worktreeId,
        'clientOperationId': clientOperationId,
        if (message != null) 'message': message,
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

  Future<Map<String, dynamic>> create({
    required String projectId,
    required String branchName,
    String? baseBranch,
  }) {
    return _http.postJson(
      baseUrl,
      '/api/mobile/workbench/worktrees/create',
      {
        'projectId': projectId,
        'branchName': branchName,
        'baseBranch': baseBranch,
      },
    );
  }

  Future<Map<String, dynamic>> remove({
    required String worktreeId,
    required String clientOperationId,
    bool force = false,
  }) {
    return _http.postJson(
      baseUrl,
      '/api/mobile/workbench/worktrees/remove',
      {
        'worktreeId': worktreeId,
        'clientOperationId': clientOperationId,
        'force': force,
      },
    );
  }

  Future<Map<String, dynamic>> repairHookFailure({
    required String worktreeId,
    required Map<String, dynamic> hookFailure,
  }) {
    return _http.postJson(
      baseUrl,
      '/api/mobile/workbench/worktrees/repair-hook-failure',
      {
        'worktreeId': worktreeId,
        'hookFailure': hookFailure,
      },
    );
  }
}
