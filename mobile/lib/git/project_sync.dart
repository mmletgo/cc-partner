// 同仓库跨设备主分支同步的纯规划（对齐 web workbenchProjectMainSync）。
//
// Business Logic（为什么需要这个模块）:
//   Git 页「同步」要把当前仓库主分支推到 origin，再在其他设备的主工作区拉取；
//   目标集合必须按 Git remote fingerprint 分组，且排除本机其它 clone。
//
// Code Logic（这个模块做什么）:
//   纯函数：项目分组键、其他设备兄弟项目、主 worktree 选取、按钮门控与摘要文案。
import 'client.dart';

/// Business Logic: 非空 fingerprint 相同才算同一仓库；空/缺失用项目 id，避免无 remote 目录互并。
/// Code Logic: trim gitRemoteFingerprint；非空 → `fp:{fingerprint}`，否则 `id:{project.id}`。
String projectGroupKey(Map<String, dynamic> project) {
  final fingerprint = (project['gitRemoteFingerprint'] as String?)?.trim() ?? '';
  if (fingerprint.isNotEmpty) {
    return 'fp:$fingerprint';
  }
  return 'id:${project['id']}';
}

/// Business Logic: 同步只打「别的设备」上的同一仓库，避免把同机第二条 clone 当成远端。
/// Code Logic: 纯函数——group key 以 fp: 开头、deviceId 不同于当前项目且 group key 相同。
List<Map<String, dynamic>> siblingProjectsOnOtherDevices(
  Map<String, dynamic>? current,
  List<Map<String, dynamic>> projects,
) {
  if (current == null) {
    return const [];
  }
  final key = projectGroupKey(current);
  if (!key.startsWith('fp:')) {
    return const [];
  }
  final currentDevice = current['deviceId'];
  return [
    for (final project in projects)
      if (project['id'] != current['id'] &&
          project['deviceId'] != null &&
          project['deviceId'] != currentDevice &&
          projectGroupKey(project) == key)
        project,
  ];
}

/// Business Logic: 同步对象是仓库主工作区，不是当前功能分支 worktree。
/// Code Logic: 返回 isMain 的第一项，没有则 null。
Map<String, dynamic>? pickMainWorktree(List<Map<String, dynamic>> worktrees) {
  for (final tree in worktrees) {
    if (tree['isMain'] == true) {
      return tree;
    }
  }
  return null;
}

/// Business Logic: 没有其他设备、主分支不能 push、busy 或 unknown 锁定时，同步按钮必须禁用。
/// Code Logic: siblingCount>0 且主 worktree 有分支且 status.canPush 且未 busy/未锁定。
bool canSyncProjectMain({
  required Map<String, dynamic>? mainWorktree,
  required int siblingCount,
  required bool busy,
  required bool actionLocked,
}) {
  if (busy || actionLocked) {
    return false;
  }
  if (siblingCount <= 0) {
    return false;
  }
  final main = mainWorktree;
  final status = WorktreeGitStatus.of(main ?? const {});
  return (main?['branch'] as String?)?.isNotEmpty == true && status.canPush;
}

/// Business Logic: 同步完成后用户要一眼看到哪些设备成功、哪些失败（对齐 web syncSucceeded/syncPartial）。
/// Code Logic: 无失败 → 「已推送主分支，并在 X · Y 上拉取」；有失败 → 成功/失败两段；成功为空显示「无」。
String syncSummaryText(List<String> pulled, List<String> failed) {
  if (failed.isEmpty) {
    return '已推送主分支，并在 ${pulled.isEmpty ? '无' : pulled.join(' · ')} 上拉取';
  }
  return '已推送主分支。已拉取：${pulled.isEmpty ? '无' : pulled.join(' · ')}。'
      '失败：${failed.join(' · ')}';
}
