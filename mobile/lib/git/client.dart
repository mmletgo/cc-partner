import '../core/lan_http.dart';
import 'mutation.dart';

/// worktree 分支名固定前缀（对齐 web WORKTREE_BRANCH_PREFIXES）。
const kWorktreeBranchPrefixes = <String>[
  'feature',
  'fix',
  'chore',
  'docs',
  'refactor',
  'test',
  'hotfix',
];

/// 创建 worktree 的默认分支前缀（对齐 web DEFAULT_WORKTREE_BRANCH_PREFIX）。
const kDefaultWorktreeBranchPrefix = 'feature';

/// Business Logic: 新建 worktree 时分支类型由固定前缀选择，用户只负责任务后缀
/// （对齐 web composeWorktreeBranchName）。
/// Code Logic: 后缀 trim 后非空返回 `prefix/suffix`，否则 null（禁止创建无名分支）。
String? composeWorktreeBranchName(String prefix, String suffix) {
  final trimmed = suffix.trim();
  if (trimmed.isEmpty) {
    return null;
  }
  return '$prefix/$trimmed';
}

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
    String? clientOperationId,
  }) {
    return _http.postJson(
      baseUrl,
      '/api/mobile/workbench/worktrees/pull',
      {
        'projectId': projectId,
        'worktreeId': worktreeId,
        if (clientOperationId != null) 'clientOperationId': clientOperationId,
      },
    );
  }

  Future<Map<String, dynamic>> push({
    required String projectId,
    required String worktreeId,
    String? clientOperationId,
  }) {
    return _http.postJson(
      baseUrl,
      '/api/mobile/workbench/worktrees/push',
      {
        'projectId': projectId,
        'worktreeId': worktreeId,
        if (clientOperationId != null) 'clientOperationId': clientOperationId,
      },
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

  /// Business Logic: unknown mutation 后必须用同一 clientOperationId 查 owning ledger
  /// 取 intent/state 对账，禁止盲重放（对齐 web git.getMutationOperation）。
  /// Code Logic: POST /api/mobile/workbench/worktrees/mutation-operation {clientOperationId}；
  /// 后端可能返回 null（无记录），非对象响应一律回 null。
  Future<Map<String, dynamic>?> mutationOperation(String clientOperationId) async {
    final decoded = await _http.postDynamic(
      baseUrl,
      '/api/mobile/workbench/worktrees/mutation-operation',
      {'clientOperationId': clientOperationId},
    );
    if (decoded is! Map) {
      return null;
    }
    return Map<String, dynamic>.from(decoded);
  }

  /// Business Logic: 「同步」按钮需要全量项目清单（含 deviceId/gitRemoteFingerprint）
  /// 来找其他设备上的同仓库兄弟项目（对齐 web projects 列表）。
  /// Code Logic: GET /api/mobile/workbench/projects/list，返回原始项目 DTO 列表（宽容解析）。
  Future<List<Map<String, dynamic>>> listAllProjects() async {
    final decoded = await _http.getDynamic(
      baseUrl,
      '/api/mobile/workbench/projects/list',
    );
    return asObjectList(decoded, wrapKey: 'projects');
  }
}

/// Business Logic: mutation 返回 unknown（envelope 或传输异常）后，必须用同一 clientOperationId
/// 查 owning ledger + 刷新权威 worktree 列表对账，禁止盲重放（对齐 web getMutationOperation +
/// reconcileWorkbenchMutation；merge/collectMerge 额外取主分支提交集作 authority）。
/// Git 页 / 终端页 / worktrees 页 / 切换条共用同一对账通道，保证裁决口径一致。
/// Code Logic:
///   1. mutation-operation 查 ledger（异常按缺失处理）；
///   2. listWorktrees 刷新权威列表（结果经 onTrees 回调给调用方更新 UI，异常保留空表）；
///   3. intent 为 merge/collectMerge 时拉主 worktree 最近 100 条提交作 authority（失败不猜成功）；
///   4. reconcileGitMutation 纯矩阵裁决（ledger 终态优先）。
Future<GitMutationReconcile> reconcileWorktreeMutation({
  required GitClient client,
  required String projectId,
  required String operationId,
  void Function(List<Map<String, dynamic>> trees)? onTrees,
}) async {
  Map<String, dynamic>? ledger;
  try {
    ledger = await client.mutationOperation(operationId);
  } catch (_) {
    ledger = null;
  }
  List<Map<String, dynamic>> trees = const [];
  try {
    final body = await client.listWorktrees(projectId, includeGitStatus: true);
    trees = asObjectList(body, wrapKey: 'worktrees');
    onTrees?.call(trees);
  } catch (_) {}
  List<String>? mainCommitHashes;
  final intent = ledger?['intent'];
  final kind =
      parseGitMutationKind(intent is Map ? intent['kind'] as String? : null);
  if (kind == GitMutationKind.merge || kind == GitMutationKind.collectMerge) {
    Map<String, dynamic>? main;
    for (final tree in trees) {
      if (tree['isMain'] == true) {
        main = tree;
        break;
      }
    }
    final mainId = main?['id'] as String?;
    if (main != null && mainId != null && mainId.isNotEmpty) {
      try {
        final commits = await client.commits(projectId, worktreeId: mainId, limit: 100);
        mainCommitHashes = [for (final commit in commits) commit.hash];
      } catch (_) {
        mainCommitHashes = null;
      }
    }
  }
  return reconcileGitMutation(
    ledger: ledger,
    worktrees: trees,
    mainCommitHashes: mainCommitHashes,
  );
}
