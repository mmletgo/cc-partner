/// 终端内 Git 提交/合并的 mutation envelope 宽容解析与 hook 失败展示纯函数。
///
/// Business Logic（为什么需要这个模块）:
///   终端 FAB 的 commit/merge 与桌面 Git 面板共用同一 mutation envelope
///   （succeeded | unknown | failedHook，对齐 Rust WorkbenchMutationEnvelopeDto）。
///   手机端需要把 envelope 稳定解析成可展示结果：成功提示、unknown 只提示对账、
///   failedHook 弹「让 AI 修复」卡。JSON 字段形态多样（camel/snake、非 Error 抛出），
///   解析必须宽容且可单测。
///
/// Code Logic（这个模块做什么）:
///   纯函数：envelope kind 判定、hookFailure 宽容视图、stdout/stderr 拼接、
///   clientOperationId 生成。无 IO。
library;

import 'dart:math';

/// mutation envelope 解析结果。
enum GitMutationOutcomeKind { succeeded, unknown, failedHook, malformed }

/// failedHook envelope 的宽容视图（对齐 web WorkbenchHookFailure）。
class HookFailureView {
  const HookFailureView({
    this.stage = '',
    this.stdout = '',
    this.stderr = '',
    this.exitCode,
  });

  /// 钩子阶段：'preCommit' | 'prePush'；未知阶段按 commit 处理。
  final String stage;

  final String stdout;
  final String stderr;
  final int? exitCode;

  /// 宽容解析：camelCase/snake_case 双读，非对象输入返回空视图。
  factory HookFailureView.fromJson(Object? raw) {
    if (raw is! Map) {
      return const HookFailureView();
    }
    return HookFailureView(
      stage: raw['stage'] as String? ?? '',
      stdout: raw['stdout'] as String? ?? '',
      stderr: raw['stderr'] as String? ?? '',
      exitCode: (raw['exitCode'] as num?)?.toInt() ?? (raw['exit_code'] as num?)?.toInt(),
    );
  }

  /// 卡片标题按 stage 区分：pre-push 用 push 标题并走 error 风格（对齐 web role=alert）。
  bool get isPush => stage == 'prePush';

  /// 展示输出：两端都有则换行拼接，只保留非空端（对齐 web formatMobileHookRepairOutput）。
  String get formattedOutput {
    final out = stdout.trim();
    final err = stderr.trim();
    if (out.isNotEmpty && err.isNotEmpty) {
      return '$out\n$err';
    }
    return out.isNotEmpty ? out : err;
  }
}

/// 解析后的 mutation envelope。
class GitMutationOutcome {
  const GitMutationOutcome({
    required this.kind,
    this.hookFailure,
    this.clientOperationId,
  });

  final GitMutationOutcomeKind kind;
  final HookFailureView? hookFailure;
  final String? clientOperationId;

  /// 宽容解析：kind 兼容 'status'/'state' 字段与 snake_case 值；
  /// 有 hookFailure 载荷但 kind 缺失时按 failedHook 兜底。
  factory GitMutationOutcome.fromJson(Map<String, dynamic> json) {
    final rawKind = (json['kind'] ?? json['status'] ?? json['state'] ?? '')
        .toString()
        .replaceAll('_', '');
    final hookRaw = json['hookFailure'] ?? json['hook_failure'];
    final opId = json['clientOperationId'] as String? ??
        json['client_operation_id'] as String?;
    if (rawKind == 'succeeded' || rawKind == 'success') {
      return GitMutationOutcome(kind: GitMutationOutcomeKind.succeeded, clientOperationId: opId);
    }
    if (rawKind == 'failedHook' || rawKind == 'failedhook') {
      return GitMutationOutcome(
        kind: GitMutationOutcomeKind.failedHook,
        hookFailure: HookFailureView.fromJson(hookRaw),
        clientOperationId: opId,
      );
    }
    if (rawKind == 'unknown') {
      return GitMutationOutcome(kind: GitMutationOutcomeKind.unknown, clientOperationId: opId);
    }
    if (rawKind.isEmpty && hookRaw is Map) {
      // 后端只回 hookFailure 载荷时按 failedHook 兜底，保证「让 AI 修复」入口不丢。
      return GitMutationOutcome(
        kind: GitMutationOutcomeKind.failedHook,
        hookFailure: HookFailureView.fromJson(hookRaw),
        clientOperationId: opId,
      );
    }
    return GitMutationOutcome(kind: GitMutationOutcomeKind.malformed, clientOperationId: opId);
  }
}

/// 业务逻辑：mutation 幂等要求同一语义操作在重试间复用同一 clientOperationId。
///
/// Code Logic：时间戳 + 随机后缀，格式 `mobile-<动作>-<ms>-<rand>`，无外部依赖可注入时钟。
String buildClientOperationId(String action, {int? nowMs, Random? random}) {
  final ms = nowMs ?? DateTime.now().millisecondsSinceEpoch;
  final rand = random ?? Random(ms);
  return 'mobile-$action-$ms-${rand.nextInt(0x7fffffff)}';
}
