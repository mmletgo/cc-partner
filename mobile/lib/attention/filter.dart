/// One Inbox item as returned by `/api/mobile/attention/v2`.
class AttentionItem {
  const AttentionItem({
    required this.id,
    required this.sourceKind,
    required this.targetKind,
    this.title,
    this.projectId,
    this.sessionId,
    this.worktreeId,
  });

  final String id;
  final String sourceKind;
  final String targetKind;
  final String? title;
  final String? projectId;
  final String? sessionId;
  final String? worktreeId;

  factory AttentionItem.fromJson(Map<String, dynamic> json) {
    final target = json['target'];
    final targetMap = target is Map<String, dynamic>
        ? target
        : (target is Map ? Map<String, dynamic>.from(target) : const <String, dynamic>{});
    return AttentionItem(
      id: json['id'] as String? ?? '',
      sourceKind: json['sourceKind'] as String? ?? '',
      targetKind: targetMap['kind'] as String? ?? '',
      title: json['title'] as String?,
      projectId: targetMap['projectId'] as String? ?? json['projectId'] as String?,
      sessionId: targetMap['terminalSessionId'] as String? ??
          targetMap['sessionId'] as String?,
      worktreeId: targetMap['worktreeId'] as String?,
    );
  }
}

/// Mobile Inbox hides tmux/dependency items (no install UI on the phone).
bool isMobileHiddenAttentionItem(AttentionItem item) {
  if (item.sourceKind == 'workbenchDependency') {
    return true;
  }
  return item.targetKind == 'settings';
}

List<AttentionItem> filterMobileInboxAttentionItems(
  Iterable<AttentionItem> items,
) {
  return items.where((item) => !isMobileHiddenAttentionItem(item)).toList();
}

/// Navigation target only — the app never executes Deliver/Retry/install from Inbox.
class AttentionNavigation {
  const AttentionNavigation({
    required this.panel,
    this.projectId,
    this.sessionId,
    this.worktreeId,
  });

  final String panel;
  final String? projectId;
  final String? sessionId;
  final String? worktreeId;
}

AttentionNavigation navigateAttention(AttentionItem item) {
  if (item.targetKind == 'agentSession' || item.sourceKind == 'agentNeedsInput') {
    return AttentionNavigation(
      panel: 'terminal',
      projectId: item.projectId,
      sessionId: item.sessionId,
      worktreeId: item.worktreeId,
    );
  }
  if (item.targetKind == 'orchestratorTask' || item.targetKind == 'orchestratorOutbox') {
    return AttentionNavigation(panel: 'attention', projectId: item.projectId);
  }
  return AttentionNavigation(panel: 'attention', projectId: item.projectId);
}
