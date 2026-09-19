/// Pick the active worktree after a project/worktree list refresh.
///
/// A new project must not keep the previous project's id, even if the string
/// happens to appear in the new list.
String? resolveActiveWorktreeId({
  required List<Map<String, dynamic>> trees,
  required String? previousId,
  required bool projectChanged,
}) {
  if (!projectChanged && previousId != null) {
    for (final tree in trees) {
      if (tree['id'] == previousId) {
        return previousId;
      }
    }
  }
  Map<String, dynamic>? preferred;
  for (final tree in trees) {
    if (tree['isMain'] == true) {
      preferred = tree;
      break;
    }
  }
  preferred ??= trees.isEmpty ? null : trees.first;
  return preferred?['id'] as String?;
}

/// Leaving the project context drops the worktree selection.
String? clearWorktreeOnLeaveProject() => null;
