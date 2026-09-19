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
    this.category,
    this.summary,
    this.updatedAt,
    this.readAt,
    this.freshness,
    this.projectName,
    this.deviceName,
  });

  final String id;
  final String sourceKind;
  final String targetKind;
  final String? title;
  final String? projectId;
  final String? sessionId;
  final String? worktreeId;

  /// 分类（decision/blocked/environment），旧后端可能缺省。
  final String? category;

  /// 摘要文案；旧后端可能缺省。
  final String? summary;

  /// 最近更新时间（RFC3339），本地日过滤依据。
  final String? updatedAt;

  /// 本设备视角的已读时间；为空即未读。
  final String? readAt;

  /// live/cached；旧后端可能缺省。
  final String? freshness;

  /// 所属项目名（快照内嵌 project 引用），用于条目 meta 展示。
  final String? projectName;

  /// 来源设备名，用于条目 meta 展示。
  final String? deviceName;

  /// Business Logic: 未读条目要加粗提示，判断口径必须与服务端 readAt 一致。
  /// Code Logic: readAt 缺失或空串视为未读（与 web isAttentionItemUnread 相同）。
  bool get isUnread => readAt == null || readAt!.isEmpty;

  factory AttentionItem.fromJson(Map<String, dynamic> json) {
    final target = json['target'];
    final targetMap = target is Map<String, dynamic>
        ? target
        : (target is Map ? Map<String, dynamic>.from(target) : const <String, dynamic>{});
    final project = json['project'];
    final projectMap =
        project is Map ? Map<String, dynamic>.from(project) : const <String, dynamic>{};
    final device = json['device'];
    final deviceMap =
        device is Map ? Map<String, dynamic>.from(device) : const <String, dynamic>{};
    return AttentionItem(
      id: json['id'] as String? ?? '',
      sourceKind: json['sourceKind'] as String? ?? '',
      targetKind: targetMap['kind'] as String? ?? '',
      title: json['title'] as String?,
      projectId: targetMap['projectId'] as String? ?? json['projectId'] as String?,
      sessionId: targetMap['terminalSessionId'] as String? ??
          targetMap['sessionId'] as String?,
      worktreeId: targetMap['worktreeId'] as String?,
      category: json['category'] as String?,
      summary: json['summary'] as String?,
      updatedAt: json['updatedAt'] as String?,
      readAt: json['readAt'] as String?,
      freshness: json['freshness'] as String?,
      projectName: projectMap['name'] as String?,
      deviceName: deviceMap['name'] as String?,
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

/// 本地日分桶结果：today 落在 now 的本地日历日，earlier 为其余条目。
class AttentionDayPartition {
  const AttentionDayPartition({required this.today, required this.earlier});

  final List<AttentionItem> today;
  final List<AttentionItem> earlier;
}

/// Business Logic: 待处理按用户本地「今天」过滤，不能用 UTC 日期把跨午夜条目判成过期。
/// Code Logic: 解析 ISO 后比较本地年/月/日；非法或缺省时间 fail-open 视为当天，避免误藏。
bool isIsoTimestampOnLocalDay(String? iso, DateTime now) {
  if (iso == null || iso.isEmpty) {
    return true;
  }
  final parsed = DateTime.tryParse(iso);
  if (parsed == null) {
    return true;
  }
  final local = parsed.isUtc ? parsed.toLocal() : parsed;
  return local.year == now.year && local.month == now.month && local.day == now.day;
}

/// Business Logic: 列表与导航徽章需要一次切分出当天与更早条目，供默认隐藏与展开共用。
/// Code Logic: 按 updatedAt 是否落在 now 的本地日历日分桶，保持原相对顺序。
AttentionDayPartition partitionAttentionItemsByLocalDay(
  List<AttentionItem> items,
  DateTime now,
) {
  final today = <AttentionItem>[];
  final earlier = <AttentionItem>[];
  for (final item in items) {
    if (isIsoTimestampOnLocalDay(item.updatedAt, now)) {
      today.add(item);
    } else {
      earlier.add(item);
    }
  }
  return AttentionDayPartition(today: today, earlier: earlier);
}

/// Business Logic: 未读数徽章与「全部已读」可用性需要统一的未读统计口径。
/// Code Logic: readAt 为空即未读，直接计数。
int countUnreadAttentionItems(Iterable<AttentionItem> items) {
  return items.where((item) => item.isUnread).length;
}

/// Business Logic: 导航徽章必须与默认「今天」列表同口径，只提示当天未处理事项。
/// Code Logic: 先按本地日切桶，再统计 today 桶内未读数。
int countTodayUnreadAttentionItems(List<AttentionItem> items, DateTime now) {
  return countUnreadAttentionItems(partitionAttentionItemsByLocalDay(items, now).today);
}

/// Business Logic: 分类 tag 必须是可见中文文案，颜色只能辅助。
/// Code Logic: 映射后端 category enum；未知分类返回 null（调用方省略 tag）。
String? attentionCategoryLabel(String? category) {
  switch (category) {
    case 'decision':
      return '决策';
    case 'blocked':
      return '阻塞';
    case 'environment':
      return '环境';
    default:
      return null;
  }
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
  if (item.targetKind == 'orchestratorTask' ||
      item.targetKind == 'orchestratorOutbox' ||
      item.targetKind == 'remoteOutbox' ||
      item.targetKind == 'experiment') {
    return AttentionNavigation(panel: 'automation', projectId: item.projectId);
  }
  return AttentionNavigation(panel: 'attention', projectId: item.projectId);
}
