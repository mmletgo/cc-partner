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
    this.cachedAt,
    this.taskId,
    this.outboxId,
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

  /// live/cached；旧后端可能缺省（非 cached 一律按实时展示）。
  final String? freshness;

  /// cached 条目的缓存时间；用于「最后同步于 …」展示。
  final String? cachedAt;

  /// orchestratorTask 目标的任务 id，跳自动化面板时用于聚焦。
  final String? taskId;

  /// remoteOutbox/orchestratorOutbox 目标的发件 id，跳自动化面板时用于聚焦。
  final String? outboxId;

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
      cachedAt: json['cachedAt'] as String?,
      taskId: targetMap['taskId'] as String?,
      outboxId: targetMap['outboxId'] as String?,
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

/// Business Logic: 分类 tag 与分组标题必须是可见中文文案，颜色只能辅助。
/// Code Logic: 映射后端 category enum（与 web i18n 文案一致）；
/// 未知分类返回 null（调用方条目省略 tag、归入「其他」组）。
String? attentionCategoryLabel(String? category) {
  switch (category) {
    case 'decision':
      return '需要你的决定';
    case 'blocked':
      return '运行受阻';
    case 'environment':
      return '环境受阻';
    default:
      return null;
  }
}

/// 固定分组顺序：需要决策 → 运行受阻 → 环境受阻（对齐 web ATTENTION_CATEGORY_ORDER）。
const attentionCategoryOrder = ['decision', 'blocked', 'environment'];

/// 未知/缺省 category 条目的尾随分组 key。
const attentionOtherCategory = 'other';

/// Business Logic: Inbox 页面按分类分组渲染，未知分类不能让条目消失。
/// Code Logic: 按 attentionCategoryOrder 分桶（空组省略），未知/缺省 category
/// 条目尾随到「其他」组，组内保持原相对顺序。
List<AttentionCategoryGroup> groupAttentionItems(List<AttentionItem> items) {
  final buckets = <String, List<AttentionItem>>{
    for (final category in attentionCategoryOrder) category: <AttentionItem>[],
    attentionOtherCategory: <AttentionItem>[],
  };
  for (final item in items) {
    final key = attentionCategoryLabel(item.category) != null
        ? item.category!
        : attentionOtherCategory;
    buckets[key]!.add(item);
  }
  return [
    for (final entry in buckets.entries)
      if (entry.value.isNotEmpty)
        AttentionCategoryGroup(category: entry.key, items: List.of(entry.value)),
  ];
}

/// 单组 Attention 条目（category 为三分类之一或 `other`）。
class AttentionCategoryGroup {
  const AttentionCategoryGroup({required this.category, required this.items});

  final String category;
  final List<AttentionItem> items;
}

/// Business Logic: 分组标题与条目 tag 共用同一文案来源，未知分类归「其他」。
/// Code Logic: 三分类映射中文标题；`other` → 其他。
String attentionGroupLabel(String category) {
  if (category == attentionOtherCategory) {
    return '其他';
  }
  return attentionCategoryLabel(category) ?? '其他';
}

/// Business Logic: freshness 徽章需要可见中文文案；非 cached 一律按实时展示。
/// Code Logic: cached → 远端缓存，其余（live/缺省/未知）→ 实时，与 web 相同。
String attentionFreshnessLabel(String? freshness) =>
    freshness == 'cached' ? '远端缓存' : '实时';

/// Business Logic: Inbox 条目 meta 行的来源动作文案必须是可见中文，不能直接拼
/// 英文枚举 sourceKind；映射对齐 web `attention:action.*`（getAttentionActionI18nKey）。
/// Code Logic: sourceKind → 动作文案的固定映射（与 web zh/attention.json 全量一致）；
/// 未知/缺省 sourceKind 回退原值，保证新增来源不丢信息。
String attentionSourceKindActionLabel(String? sourceKind) {
  switch (sourceKind) {
    case 'orchestratorHumanReview':
      return '前往复核';
    case 'orchestratorBlocked':
      return '查看阻塞原因';
    case 'remoteOutboxFailed':
      return '查看失败项';
    case 'workbenchDependency':
      return '打开设置';
    case 'agentNeedsInput':
    case 'agentFailed':
      return '打开终端';
    case 'experimentNeedsDecision':
      return '查看实验';
    case 'agentHubConflict':
    case 'agentHubProjectionBlocked':
      return '打开 Agent Hub';
    default:
      // 未知来源回退原值（空串返回空串，meta 行由调用方决定省略）。
      return sourceKind ?? '';
  }
}

/// Navigation target only — the app never executes Deliver/Retry/install from Inbox.
class AttentionNavigation {
  const AttentionNavigation({
    required this.panel,
    this.projectId,
    this.sessionId,
    this.worktreeId,
    this.taskId,
    this.outboxId,
  });

  final String panel;
  final String? projectId;
  final String? sessionId;
  final String? worktreeId;

  /// 跳自动化面板时聚焦的任务 id（orchestratorTask）。
  final String? taskId;

  /// 跳自动化面板时聚焦的发件 id（remoteOutbox/orchestratorOutbox）。
  final String? outboxId;
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
  if (item.targetKind == 'orchestratorTask') {
    return AttentionNavigation(
      panel: 'automation',
      projectId: item.projectId,
      taskId: item.taskId,
    );
  }
  if (item.targetKind == 'orchestratorOutbox' ||
      item.targetKind == 'remoteOutbox') {
    return AttentionNavigation(
      panel: 'automation',
      projectId: item.projectId,
      outboxId: item.outboxId,
    );
  }
  if (item.targetKind == 'experiment') {
    // experiment 无移动端聚焦实体，仅导航到自动化面板（与 web 一致）。
    return AttentionNavigation(panel: 'automation', projectId: item.projectId);
  }
  return AttentionNavigation(panel: 'attention', projectId: item.projectId);
}
