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
  final void Function(ProjectSummary project, String panel, String? sessionId) onNavigate;

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

  /// 拉取完整快照（下拉刷新/首载用）。
  Future<void> _reload() async {
    setState(() {
      _loading = true;
      _error = null;
    });
    try {
      final items = await _client.listVisible();
      if (mounted) {
        setState(() {
          _items = items;
          _now = DateTime.now();
          _loading = false;
        });
        widget.onItemsChanged?.call(items);
      }
    } catch (error) {
      if (mounted) {
        setState(() {
          _error = error.toString();
          _loading = false;
        });
      }
    }
  }

  /// 用服务端返回的最新快照覆盖本地列表（不整页重载）。
  void _applyItems(List<AttentionItem> items) {
    setState(() {
      _items = items;
      _now = DateTime.now();
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

  /// Business Logic: 点击条目应直达权威界面，且已读状态随之落库（失败不阻断导航）。
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
    widget.onNavigate(project, nav.panel, nav.sessionId);
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
            Center(child: Text('没有待处理事项。')),
          ],
        ),
      );
    }
    final partition = partitionAttentionItemsByLocalDay(_items, _now);
    final visible = _showEarlier ? _items : partition.today;
    final unreadTotal = countUnreadAttentionItems(_items);
    return RefreshIndicator(
      onRefresh: _reload,
      child: ListView(
        physics: const AlwaysScrollableScrollPhysics(),
        padding: const EdgeInsets.symmetric(vertical: 4),
        children: [
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
          for (final item in visible) _itemCard(context, item),
        ],
      ),
    );
  }

  /// Business Logic: 未读条目要一眼可辨，且单条已读/未读可随时切换。
  /// Code Logic: 未读标题加粗 + 圆点；副标题给分类 tag、来源与「项目·设备·时间」meta；
  /// trailing 按钮做 toggle，进行中禁用。
  Widget _itemCard(BuildContext context, AttentionItem item) {
    final theme = Theme.of(context);
    final unread = item.isUnread;
    final pending = _pendingIds.contains(item.id);
    final category = attentionCategoryLabel(item.category);
    final title = item.title ?? item.id;
    final meta = _metaLabel(item);
    final time = _formatTime(item.updatedAt);
    final metaParts = <String>[
      if (meta != null) meta,
      if (time.isNotEmpty) time,
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
            Text(
              [
                if (category != null) category,
                item.sourceKind,
              ].join(' · '),
              style: theme.textTheme.labelSmall,
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
}
