import 'package:flutter/material.dart';

import '../address_book/book.dart';
import '../attention/client.dart';
import '../attention/filter.dart';
import '../core/lan_http.dart';
import '../projects/client.dart';

class AttentionPage extends StatefulWidget {
  const AttentionPage({
    super.key,
    required this.book,
    required this.http,
    required this.onNavigate,
    this.onItemsChanged,
    this.attentionClient,
    this.projectsClient,
  });

  final AddressBook book;
  final LanHttpClient http;

  /// 导航回调：panel 映射保持现状（orchestratorTask/remoteOutbox/experiment → automation）；
  /// 跳 automation 时把条目对应的 taskId/outboxId 作为 focus 参数传出（字段来源见
  /// web mobileAttentionTarget.ts）。
  final void Function(
    ProjectSummary project,
    String panel,
    String? sessionId, {
    String? focusTaskId,
    String? focusOutboxId,
  }) onNavigate;

  /// 列表数据变化（加载/标记后）回调，供外层同步未读徽章。
  final void Function(List<AttentionItem> items)? onItemsChanged;

  /// 可注入的 Attention 客户端（测试用）；缺省按当前 PC 构造。
  final AttentionClient? attentionClient;

  /// 可注入的项目客户端（测试用）；缺省按当前 PC 构造。
  final ProjectsClient? projectsClient;

  @override
  State<AttentionPage> createState() => _AttentionPageState();
}

class _AttentionPageState extends State<AttentionPage> {
  List<AttentionItem> _items = [];
  String? _error;
  bool _loading = true;

  /// 本地日过滤：默认只显示今天条目。
  bool _showEarlier = false;

  /// 正在标记已读/未读的条目 id，用于禁用对应按钮。
  final Set<String> _pendingIds = {};
  bool _markAllBusy = false;

  /// 日过滤与时间展示的基准时刻，每次拉取快照时刷新。
  DateTime _now = DateTime.now();

  /// 刷新失败但已有快照时的 stale 标记（对齐 web useAttention：刷新失败只标 stale）。
  bool _stale = false;

  /// 最近一次成功拉取时间（ISO），供 stale banner 展示。
  String? _lastSucceededAt;

  late final AttentionClient _client =
      widget.attentionClient ?? AttentionClient(widget.http, widget.book.active!.baseUrl);

  /// Business Logic: 点击条目前需要确认目标项目仍在最近列表里。
  /// Code Logic: 复用注入或默认的 ProjectsClient 拉取最近项目。
  Future<List<ProjectSummary>> _listProjects() =>
      (widget.projectsClient ?? ProjectsClient(widget.http, widget.book.active!.baseUrl))
          .listRecent();

  @override
  void initState() {
    super.initState();
    _reload();
  }

  /// Business Logic: 拉取完整快照（下拉刷新/首载用）；已有快照时刷新失败只标 stale，
  /// 不得用整屏错误覆盖旧数据（对齐 web useAttention 语义）。
  /// Code Logic: 空列表失败 → 错误屏；非空失败 → stale banner + 保留旧列表；
  /// 成功 → 更新快照、成功时间并清 stale。
  Future<void> _reload() async {
    final silent = _items.isNotEmpty;
    if (mounted) {
      setState(() {
        if (!silent) {
          _loading = true;
        }
        _error = null;
      });
    }
    try {
      final items = await _client.listVisible();
      if (mounted) {
        setState(() {
          _items = items;
          _now = DateTime.now();
          _stale = false;
          _lastSucceededAt = _now.toIso8601String();
          _loading = false;
        });
        widget.onItemsChanged?.call(items);
      }
    } catch (error) {
      if (mounted) {
        if (_items.isEmpty) {
          setState(() {
            _error = error.toString();
            _loading = false;
          });
        } else {
          setState(() => _stale = true);
        }
      }
    }
  }

  /// 用服务端返回的最新快照覆盖本地列表（不整页重载）。
  void _applyItems(List<AttentionItem> items) {
    setState(() {
      _items = items;
      _now = DateTime.now();
      _stale = false;
      _lastSucceededAt = _now.toIso8601String();
    });
    widget.onItemsChanged?.call(items);
  }

  /// 单条「标为已读/未读」toggle；失败 SnackBar，成功本地更新。
  Future<void> _toggleRead(AttentionItem item) async {
    if (_pendingIds.contains(item.id)) {
      return;
    }
    setState(() => _pendingIds.add(item.id));
    try {
      final items =
          item.isUnread ? await _client.markRead([item.id]) : await _client.markUnread([item.id]);
      if (mounted) {
        setState(() => _pendingIds.remove(item.id));
        _applyItems(items);
      }
    } catch (error) {
      if (mounted) {
        setState(() => _pendingIds.remove(item.id));
        _showSnack('标记失败：$error');
      }
    }
  }

  /// 顶部「全部已读」；无未读时按钮已禁用。
  Future<void> _markAllRead() async {
    if (_markAllBusy) {
      return;
    }
    setState(() => _markAllBusy = true);
    try {
      final items = await _client.markAllRead();
      if (mounted) {
        _applyItems(items);
      }
    } catch (error) {
      if (mounted) {
        _showSnack('标记失败：$error');
      }
    } finally {
      if (mounted) {
        setState(() => _markAllBusy = false);
      }
    }
  }

  /// Business Logic: 点击条目应直达权威界面，且已读状态随之落库（失败不阻断导航）；
  /// 跳自动化面板时携带 taskId/outboxId 供聚焦。
  /// Code Logic: 未读先 markRead（best-effort），再按 navigateAttention 语义匹配项目导航；
  /// 项目已不在最近列表时 SnackBar 提示而不是静默。
  Future<void> _open(AttentionItem item) async {
    if (item.isUnread) {
      try {
        final items = await _client.markRead([item.id]);
        if (mounted) {
          _applyItems(items);
        }
      } catch (_) {
        // 标记失败不阻断导航。
      }
    }
    final nav = navigateAttention(item);
    if (nav.projectId == null) {
      return;
    }
    List<ProjectSummary> projects;
    try {
      projects = await _listProjects();
    } catch (error) {
      if (mounted) {
        _showSnack('无法获取项目列表：$error');
      }
      return;
    }
    final project = projects.where((p) => p.id == nav.projectId).firstOrNull;
    if (!mounted) {
      return;
    }
    if (project == null) {
      _showSnack('该项目已不在列表中');
      return;
    }
    widget.onNavigate(
      project,
      nav.panel,
      nav.sessionId,
      focusTaskId: nav.taskId,
      focusOutboxId: nav.outboxId,
    );
  }

  void _showSnack(String message) {
    if (!mounted) {
      return;
    }
    ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(message)));
  }

  /// 把 ISO 时间格式化为本地短时间：今天显示 HH:mm，更早显示 M-d HH:mm；解析失败显示原文。
  String _formatTime(String? iso) {
    if (iso == null || iso.isEmpty) {
      return '';
    }
    final parsed = DateTime.tryParse(iso);
    if (parsed == null) {
      return iso;
    }
    final local = parsed.isUtc ? parsed.toLocal() : parsed;
    String two(int value) => value.toString().padLeft(2, '0');
    final hm = '${two(local.hour)}:${two(local.minute)}';
    if (isIsoTimestampOnLocalDay(iso, _now)) {
      return hm;
    }
    return '${local.month}-${two(local.day)} $hm';
  }

  /// 拼「项目 · 设备」meta 文案；两者都缺省返回 null。
  String? _metaLabel(AttentionItem item) {
    final project = item.projectName?.trim() ?? '';
    final device = item.deviceName?.trim() ?? '';
    if (project.isNotEmpty && device.isNotEmpty) {
      return '$project · $device';
    }
    if (project.isNotEmpty) {
      return project;
    }
    if (device.isNotEmpty) {
      return device;
    }
    return null;
  }

  /// Business Logic: 刷新失败但已有快照时，顶部提示状态可能过期并给出最后成功时间。
  /// Code Logic: 对齐 web staleBanner + lastUpdated 文案；无成功时间时只显示主文案。
  Widget _staleBanner() {
    final theme = Theme.of(context);
    final lastUpdated = _formatTime(_lastSucceededAt);
    return Padding(
      padding: const EdgeInsets.fromLTRB(8, 4, 8, 0),
      child: Material(
        key: const Key('attention-stale-banner'),
        color: theme.colorScheme.errorContainer,
        borderRadius: BorderRadius.circular(8),
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
          child: Semantics(
            liveRegion: true,
            child: Text(
              lastUpdated.isEmpty
                  ? '状态可能已过期'
                  : '状态可能已过期 · 上次成功更新：$lastUpdated',
              style: theme.textTheme.bodySmall
                  ?.copyWith(color: theme.colorScheme.onErrorContainer),
            ),
          ),
        ),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    if (_loading) {
      return const Center(child: CircularProgressIndicator());
    }
    if (_error != null) {
      return Center(child: Text(_error!));
    }
    if (_items.isEmpty) {
      return RefreshIndicator(
        onRefresh: _reload,
        child: ListView(
          physics: const AlwaysScrollableScrollPhysics(),
          children: const [
            SizedBox(height: 120),
            Center(child: Text('当前没有阻塞工作的事项')),
          ],
        ),
      );
    }
    final partition = partitionAttentionItemsByLocalDay(_items, _now);
    final visible = _showEarlier ? _items : partition.today;
    final unreadTotal = countUnreadAttentionItems(_items);
    final groups = groupAttentionItems(visible);
    return RefreshIndicator(
      onRefresh: _reload,
      child: ListView(
        physics: const AlwaysScrollableScrollPhysics(),
        padding: const EdgeInsets.symmetric(vertical: 4),
        children: [
          if (_stale) _staleBanner(),
          Align(
            alignment: Alignment.centerRight,
            child: Padding(
              padding: const EdgeInsets.symmetric(horizontal: 8),
              child: TextButton(
                key: const Key('attention-mark-all-read'),
                onPressed: unreadTotal == 0 || _markAllBusy ? null : _markAllRead,
                child: Text(_markAllBusy ? '标记中…' : '全部已读'),
              ),
            ),
          ),
          if (partition.earlier.isNotEmpty)
            ListTile(
              key: const Key('attention-day-filter'),
              dense: true,
              title: Text(
                _showEarlier ? '收起更早条目' : '显示更早 ${partition.earlier.length} 条',
              ),
              trailing: Icon(_showEarlier ? Icons.expand_less : Icons.expand_more),
              onTap: () => setState(() => _showEarlier = !_showEarlier),
            ),
          if (visible.isEmpty)
            const Padding(
              padding: EdgeInsets.all(24),
              child: Center(child: Text('今天没有待处理事项。')),
            ),
          for (final group in groups) ...[
            Padding(
              key: Key('attention-group-${group.category}'),
              padding: const EdgeInsets.fromLTRB(16, 12, 16, 4),
              child: Text(
                attentionGroupLabel(group.category),
                style: Theme.of(context).textTheme.titleSmall,
              ),
            ),
            for (final item in group.items) _itemCard(context, item),
          ],
        ],
      ),
    );
  }

  /// Business Logic: 未读条目要一眼可辨；summary/freshness/缓存时间帮助用户判断轻重，
  /// 单条已读/未读可随时切换。
  /// Code Logic: 未读标题加粗 + 圆点；标题下 summary（最多 2 行省略）；
  /// meta 行 = freshness 徽章 + 分类 · 来源 + 「项目·设备·时间」（cached 追加最后同步于）；
  /// trailing 按钮做 toggle，进行中禁用。
  Widget _itemCard(BuildContext context, AttentionItem item) {
    final theme = Theme.of(context);
    final unread = item.isUnread;
    final pending = _pendingIds.contains(item.id);
    final category = attentionCategoryLabel(item.category);
    final title = item.title ?? item.id;
    final summary = item.summary?.trim() ?? '';
    final meta = _metaLabel(item);
    final time = _formatTime(item.updatedAt);
    final metaParts = <String>[
      if (meta != null) meta,
      if (time.isNotEmpty) time,
      if (item.freshness == 'cached' && item.cachedAt != null)
        '最后同步于 ${_formatTime(item.cachedAt)}',
    ];
    return Card(
      key: Key('attention-item-${item.id}'),
      margin: const EdgeInsets.symmetric(horizontal: 8, vertical: 4),
      child: ListTile(
        leading: unread
            ? Container(
                width: 10,
                height: 10,
                decoration:
                    BoxDecoration(color: theme.colorScheme.primary, shape: BoxShape.circle),
              )
            : Icon(Icons.check, size: 18, color: theme.colorScheme.outline),
        title: Text(
          title,
          maxLines: 2,
          overflow: TextOverflow.ellipsis,
          style: unread
              ? theme.textTheme.titleMedium?.copyWith(fontWeight: FontWeight.w600)
              : theme.textTheme.titleMedium?.copyWith(color: theme.colorScheme.outline),
        ),
        subtitle: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            const SizedBox(height: 2),
            if (summary.isNotEmpty)
              Padding(
                padding: const EdgeInsets.only(bottom: 2),
                child: Text(
                  summary,
                  maxLines: 2,
                  overflow: TextOverflow.ellipsis,
                  style: theme.textTheme.bodySmall,
                ),
              ),
            Row(
              children: [
                _freshnessChip(theme, item),
                const SizedBox(width: 6),
                Expanded(
                  child: Text(
                    [
                      if (category != null) category,
                      item.sourceKind,
                    ].join(' · '),
                    style: theme.textTheme.labelSmall,
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                  ),
                ),
              ],
            ),
            if (metaParts.isNotEmpty)
              Text(metaParts.join(' · '), style: theme.textTheme.labelSmall),
          ],
        ),
        isThreeLine: true,
        trailing: TextButton(
          key: Key('attention-toggle-read-${item.id}'),
          onPressed: pending ? null : () => _toggleRead(item),
          child: Text(unread ? '标为已读' : '标为未读'),
        ),
        onTap: () => _open(item),
      ),
    );
  }

  /// Business Logic: live/cached 需要视觉区分的徽章（对齐 web metaTag 语义）。
  /// Code Logic: cached 用中性容器色，live 用主容器色；小号 chip 纯辅助，文案可读；
  /// key 带 item id 便于测试定位。
  Widget _freshnessChip(ThemeData theme, AttentionItem item) {
    final cached = item.freshness == 'cached';
    return Container(
      key: Key('attention-freshness-${item.id}'),
      padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 1),
      decoration: BoxDecoration(
        color: cached
            ? theme.colorScheme.secondaryContainer
            : theme.colorScheme.primaryContainer,
        borderRadius: BorderRadius.circular(999),
      ),
      child: Text(
        attentionFreshnessLabel(item.freshness),
        style: theme.textTheme.labelSmall?.copyWith(
          fontSize: 10,
          color: cached ? theme.colorScheme.onSecondaryContainer : theme.colorScheme.onPrimaryContainer,
        ),
      ),
    );
  }
}
