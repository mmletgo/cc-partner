import 'dart:async';
import 'dart:io';

/// Git/worktree mutation 的相位机与 ledger 对账矩阵（对齐 web mobilePanelState +
/// workbenchMutationReconciliation，按移动端尺度裁剪）。
///
/// Business Logic（为什么需要这个模块）:
///   commit/push/pull/merge/remove 在 timeout/network 下无法确定结果，禁止盲重放；
///   必须锁定动作、用同一 clientOperationId 查 ledger + 权威列表对账出「实际已成功/未生效」。
///
/// Code Logic（这个模块做什么）:
///   纯函数与无 IO 状态机：相位推进、same-id 选择、envelope 解析、对账矩阵；
///   authority 以 worktrees.list 的存在性/主分支提交为主，commit/push/pull 无 ledger 终态时保持 unknown。

/// mutation UI 相位：空闲 / 执行中 / 对账中 / 结果未知（对齐 web MobileMutationPhase）。
enum GitMutationPhase { idle, busy, reconciling, unknown }

/// mutation 种类（ledger wire 小写 token）。
enum GitMutationKind { commit, push, pull, merge, collectMerge, remove }

/// 纯对账结果：终态成功 / 终态失败 / 仍不确定。
enum GitMutationReconcile { confirmedSucceeded, confirmedFailed, unknown }

/// Business Logic: unknown/reconciling 期间禁止用新 clientOperationId 盲重放，必须复用原 id。
/// Code Logic: phase 为 reconciling/unknown 且已有 id 时返回既有 id；否则返回 nextId。
String pickGitMutationOperationId(
  GitMutationPhase phase,
  String? existingId,
  String nextId,
) {
  if ((phase == GitMutationPhase.reconciling || phase == GitMutationPhase.unknown) &&
      existingId != null &&
      existingId.isNotEmpty) {
    return existingId;
  }
  return nextId;
}

/// Business Logic: unknown/busy/reconciling 相位必须禁用动作按钮，防止用户生成新 id 再发一次。
/// Code Logic: phase != idle 时返回 true。
bool isGitMutationActionLocked(GitMutationPhase phase) =>
    phase != GitMutationPhase.idle;

/// Business Logic: ledger/wire 的 kind 是小写字符串，宽容解析失败时回退 null（不得猜成功）。
/// Code Logic: 按名字匹配枚举，未知值返回 null。
GitMutationKind? parseGitMutationKind(String? name) {
  for (final kind in GitMutationKind.values) {
    if (kind.name == name) {
      return kind;
    }
  }
  return null;
}

/// Business Logic: ledger 终态（succeeded/failed）优先于稀疏 authority；中间态一律继续对账。
/// Code Logic: succeeded/failed 映射终态，其余（claimed/running/未知/缺失）返回 null。
GitMutationReconcile? ledgerTerminalState(Map<String, dynamic>? ledger) {
  final state = ledger?['state'];
  if (state == 'succeeded') {
    return GitMutationReconcile.confirmedSucceeded;
  }
  if (state == 'failed') {
    return GitMutationReconcile.confirmedFailed;
  }
  return null;
}

/// Business Logic: merge/remove 的 authority 判定只需要「列表里还在不在」与
/// 「主分支提交是否包含源 head」两个事实，其余维度（tree/ref）移动端拿不到就保持 unknown。
/// Code Logic: 从 intent + 最新 worktree 列表（可选主分支提交 hash 集）计算 authority 布尔值。
Map<String, dynamic> buildGitMutationAuthority({
  required Map<String, dynamic> intent,
  required List<Map<String, dynamic>> worktrees,
  List<String>? mainCommitHashes,
}) {
  final kind = intent['kind'];
  if (kind == 'merge') {
    final sourceId = intent['sourceWorktreeId'];
    final sourceHead = intent['sourceHead'] as String? ?? '';
    final sourcePresent = worktrees.any((tree) => tree['id'] == sourceId);
    bool? mainContains;
    if (mainCommitHashes != null) {
      mainContains = mainCommitHashes.any(
        (hash) =>
            hash == sourceHead ||
            (sourceHead.isNotEmpty && sourceHead.startsWith(hash)) ||
            (hash.isNotEmpty && hash.startsWith(sourceHead)),
      );
    }
    return {
      'sourceWorktreePresent': sourcePresent,
      if (mainContains != null) 'mainContainsSourceHead': mainContains,
    };
  }
  if (kind == 'collectMerge') {
    final sources = intent['sources'];
    bool? mainContains;
    if (mainCommitHashes != null && sources is List) {
      final oids = [
        for (final source in sources)
          if (source is Map) source['oid'] as String? ?? '',
      ];
      mainContains = oids.every(
        (oid) => mainCommitHashes.any(
          (hash) =>
              hash == oid ||
              (oid.isNotEmpty && oid.startsWith(hash)) ||
              (hash.isNotEmpty && hash.startsWith(oid)),
        ),
      );
    }
    return {
      if (mainContains != null) 'mainContainsSourceHead': mainContains,
    };
  }
  if (kind == 'remove') {
    final id = intent['worktreeId'];
    return {'worktreeIdentityPresent': worktrees.any((tree) => tree['id'] == id)};
  }
  return const {};
}

/// Business Logic: unknown envelope 后用 ledger intent 与刷新后的权威状态确认是否已生效；
/// 与 Rust confirm_mutation 语义对齐，绝不猜成功。
/// Code Logic:
///   1. ledger 缺失 → unknown；
///   2. ledger.state 终态优先；
///   3. intent 缺失/未知 → unknown；
///   4. remove：worktree 已不在列表 → 成功；
///      merge：源不在列表且主分支包含源 head → 成功；
///      collectMerge：主分支包含全部源 head → 成功；
///      commit/push/pull：无 ledger 终态时移动端拿不到足够 authority → 保持 unknown。
GitMutationReconcile reconcileGitMutation({
  required Map<String, dynamic>? ledger,
  required List<Map<String, dynamic>> worktrees,
  List<String>? mainCommitHashes,
}) {
  final terminal = ledgerTerminalState(ledger);
  if (terminal != null) {
    return terminal;
  }
  final intent = ledger?['intent'];
  if (intent is! Map) {
    return GitMutationReconcile.unknown;
  }
  final intentMap = Map<String, dynamic>.from(intent);
  final authority = buildGitMutationAuthority(
    intent: intentMap,
    worktrees: worktrees,
    mainCommitHashes: mainCommitHashes,
  );
  switch (intentMap['kind']) {
    case 'remove':
      return authority['worktreeIdentityPresent'] == false
          ? GitMutationReconcile.confirmedSucceeded
          : GitMutationReconcile.unknown;
    case 'merge':
      final sourceGone = authority['sourceWorktreePresent'] == false;
      final mainContains = authority['mainContainsSourceHead'];
      return (sourceGone && mainContains == true)
          ? GitMutationReconcile.confirmedSucceeded
          : GitMutationReconcile.unknown;
    case 'collectMerge':
      return authority['mainContainsSourceHead'] == true
          ? GitMutationReconcile.confirmedSucceeded
          : GitMutationReconcile.unknown;
    default:
      return GitMutationReconcile.unknown;
  }
}

/// 后端 mutation envelope（成功通道）的宽容解析。
class GitMutationEnvelope {
  const GitMutationEnvelope({
    required this.kind,
    this.value,
    this.clientOperationId,
    this.hookFailure,
  });

  /// 宽容解析：缺字段回退空 envelope（kind 为空串按未知处理）。
  factory GitMutationEnvelope.from(Object? raw) {
    if (raw is! Map) {
      return const GitMutationEnvelope(kind: '');
    }
    final map = Map<String, dynamic>.from(raw);
    return GitMutationEnvelope(
      kind: map['kind'] as String? ?? '',
      value: map['value'] is Map
          ? Map<String, dynamic>.from(map['value'] as Map)
          : null,
      clientOperationId: map['clientOperationId'] as String?,
      hookFailure: map['hookFailure'] is Map
          ? Map<String, dynamic>.from(map['hookFailure'] as Map)
          : null,
    );
  }

  final String kind;

  /// succeeded 时的权威 value（如 worktree DTO）。
  final Map<String, dynamic>? value;
  final String? clientOperationId;
  final Map<String, dynamic>? hookFailure;

  bool get succeeded => kind == 'succeeded';
  bool get unknown => kind == 'unknown';
  bool get failedHook => kind == 'failedHook';
}

/// Business Logic: 网络层异常（连接失败/超时）意味着请求可能已到达也可能没到达，
/// 必须按 unknown 处理；服务器已应答的错误（LAN HTTP 非 2xx）是确定失败。
/// Code Logic: SocketException/TimeoutException/HandshakeException 视为 unknown，其余 false。
bool isTransportUnknownError(Object error) {
  return error is SocketException ||
      error is TimeoutException ||
      error is HandshakeException ||
      error is HttpException;
}

/// 无 IO 的 mutation 相位机：git/worktrees 页共用，负责相位与 same-id 的推进。
class GitMutationTracker {
  GitMutationPhase _phase = GitMutationPhase.idle;
  String? _operationId;
  GitMutationKind? _unknownKind;
  String? _unknownWorktreeId;

  GitMutationPhase get phase => _phase;
  String? get operationId => _operationId;
  GitMutationKind? get unknownKind => _unknownKind;
  String? get unknownWorktreeId => _unknownWorktreeId;
  bool get actionLocked => isGitMutationActionLocked(_phase);

  /// Business Logic: 发起 mutation 前要拿稳定 id——unknown/reconciling 时复用旧 id，否则铸造新 id。
  /// Code Logic: pickGitMutationOperationId + 进入 busy 相位并记录 kind/worktreeId。
  String begin({
    required GitMutationKind kind,
    required String worktreeId,
    required String nextOperationId,
  }) {
    _operationId = pickGitMutationOperationId(_phase, _operationId, nextOperationId);
    _phase = GitMutationPhase.busy;
    _unknownKind = kind;
    _unknownWorktreeId = worktreeId;
    return _operationId!;
  }

  /// Business Logic: 请求返回 unknown（envelope 或传输异常）后先自动对账，期间保持锁定。
  /// Code Logic: 相位置 reconciling，保留同一 operationId。
  void beginReconcile() {
    _phase = GitMutationPhase.reconciling;
  }

  /// Business Logic: 网络异常等拿不到 envelope 时直接进入 unknown，等用户/自动对账。
  /// Code Logic: 相位置 unknown；kind/worktreeId 已在 begin 记录，可为空场景单独注入。
  void markUnknown({GitMutationKind? kind, String? worktreeId}) {
    _unknownKind ??= kind;
    _unknownWorktreeId ??= worktreeId;
    _phase = GitMutationPhase.unknown;
  }

  /// Business Logic: 对账出终态后解锁；仍 unknown 则保持锁定并保留原 id 供再次核对。
  /// Code Logic: 终态 → idle 并清空 id/kind；unknown → 相位置 unknown。
  void settleReconcile(GitMutationReconcile result) {
    if (result == GitMutationReconcile.confirmedSucceeded ||
        result == GitMutationReconcile.confirmedFailed) {
      _phase = GitMutationPhase.idle;
      _operationId = null;
      _unknownKind = null;
      _unknownWorktreeId = null;
      return;
    }
    _phase = GitMutationPhase.unknown;
  }

  /// Business Logic: 确定性失败/取消后要立即解锁，允许用户重新发起（可铸造新 id）。
  /// Code Logic: 回 idle 并清空全部记录。
  void markIdle() {
    _phase = GitMutationPhase.idle;
    _operationId = null;
    _unknownKind = null;
    _unknownWorktreeId = null;
  }

  /// Business Logic: 切项目/切 worktree 后旧 unknown 锁不得污染新上下文（对齐 web 重置 effect）。
  /// Code Logic: 等价 markIdle 的语义别名。
  void reset() => markIdle();
}
