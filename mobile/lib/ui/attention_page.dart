import 'package:flutter/material.dart';

import '../address_book/book.dart';
import '../attention/client.dart';
import '../attention/filter.dart';
import '../core/lan_http.dart';
import '../projects/client.dart';
import '../transfer/polling.dart';

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

  /// 后端缺 attention 能力的专用态：显示专用横幅并抑制 loading/error/empty。
  bool _unsupported = false;

  /// 本地日过滤：默认只显示今天条目。
  bool _showEarlier = false;

  /// 正在标记已读/未读的条目 id，用于禁用对应按钮。
  final Set<String> _pendingIds = {};
  bool _markAllBusy = false;

  /// 是否有刷新请求在途（single-flight 门闩，防止轮询/手动刷新重入）。
  bool _refreshInFlight = false;

  /// 请求序号守卫：每次刷新自增并捕获，响应回来时序号不一致则丢弃（防过期覆盖）。
  int _refreshSeq = 0;

  /// 头部「刷新」按钮 busy 态：刷新进行中禁用并显示「刷新中…」。
  bool _refreshing = false;

  /// 可见时轮询：对齐 web useVisibilityPolling——App 处于 resumed 时每 10s
  /// 静默拉一次快照，hidden/inactive 暂停，回前台立即补拉。
  late final VisibilityPoller _poller;

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
    // 可见时轮询：10s 周期对齐 web；initState 已首拉一次，不再立即重复执行，
    // 回前台的立即补拉由 poller 生命周期回调承担。
    _poller = VisibilityPoller(
      interval: const Duration(seconds: 10),
      task: _pollRefresh,
    );
    _reload();
    _poller.start(runImmediately: false);
  }

  @override
  void dispose() {
    _poller.dispose();
    super.dispose();
  }

  /// Business Logic: 下拉刷新/首载/重试共用的刷新入口；已有快照时刷新失败只标 stale，
  /// 不得用整屏错误覆盖旧数据（对齐 web useAttention 语义）。
  /// Code Logic: 按是否已有快照推导 silent（有快照静默、无快照进 loading），委托 _refresh。
  Future<void> _reload() => _refresh(silent: _items.isNotEmpty);

  /// Business Logic: 前台轮询需无感更新快照，让侧栏未读徽章不滞后于 web；
  /// 失败不打扰用户（有快照走 stale 横幅，无快照维持错误屏现状）。
  /// Code Logic: 轮询路径恒为静默，委托 _refresh。
  Future<void> _pollRefresh() => _refresh(silent: true);

  /// Business Logic: 拉取完整快照（首载/下拉/头部刷新按钮/轮询/重试共用同一刷新路径）；
  /// 已有快照时刷新失败只标 stale，不得用整屏错误覆盖旧数据（对齐 web useAttention）。
  /// Code Logic: single-flight 防重入 + 请求序号守卫丢弃过期响应；silent 不进 loading；
  /// 空列表失败 → 错误屏；非空失败 → stale banner + 保留旧列表；成功 → 更新快照、
  /// 记录成功时间并清 stale。
  Future<void> _refresh({required bool silent}) async {
    if (_refreshInFlight) {
      return;
    }
    _refreshInFlight = true;
    final seq = ++_refreshSeq;
    if (mounted) {
      setState(() {
        if (!silent) {
          _loading = true;
          _error = null;
        }
        _refreshing = true;
      });
    }
    try {
      final items = await _client.listVisible();
      if (!mounted || seq != _refreshSeq) {
        return;
      }
      setState(() {
        _items = items;
        _now = DateTime.now();
        _stale = false;
        _unsupported = false;
        _lastSucceededAt = _now.toIso8601String();
        _loading = false;
      });
      widget.onItemsChanged?.call(items);
    } catch (error) {
      if (!mounted || seq != _refreshSeq) {
        return;
      }
      if (error is AttentionUnsupportedError) {
        // 后端缺 attention.v1：专用态，抑制整屏错误与空态（对齐 web unsupported）。
        setState(() {
          _unsupported = true;
          _error = null;
          _loading = false;
          _stale = false;
        });
      } else if (_items.isEmpty) {
        setState(() {
          _error = error.toString();
          _loading = false;
        });
      } else {
        setState(() => _stale = true);
      }
    } finally {
      _refreshInFlight = false;
      if (mounted && seq == _refreshSeq) {
        setState(() => _refreshing = false);
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

  /// Business Logic: 点击条目应直达权威界面，且已读状态随之落库（失败提示但不
  /// 阻断导航）；跳自动化面板时携带 taskId/outboxId 供聚焦。
  /// Code Logic: 未读先 markRead，失败 SnackBar 提示后继续导航（对齐 web markError
  /// 横幅不拦跳转），再按 navigateAttention 语义匹配项目导航；项目已不在最近列表
  /// 时 SnackBar 提示而不是静默。
  Future<void> _open(AttentionItem item) async {
    if (item.isUnread) {
      try {
        final items = await _client.markRead([item.id]);
        if (mounted) {
          _applyItems(items);
        }
      } catch (error) {
        // 标记失败不阻断导航，但要明确提示（不再静默吞掉）。
        _showSnack('标记失败：$error');
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
      // 初始加载失败（无快照）的错误屏：保持错误文案，并提供「重试」走同一刷新路径
      // （对齐 web 错误态 reload 按钮）。
      return Center(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Padding(
              padding: const EdgeInsets.symmetric(horizontal: 24),
              child: Text(_error!, textAlign: TextAlign.center),
            ),
            const SizedBox(height: 8),
            TextButton(
              key: const Key('attention-retry'),
              onPressed: _reload,
              child: const Text('重试'),
            ),
          ],
        ),
      );
    }
    if (_items.isEmpty) {
      // 后端缺 attention 能力：专用横幅 + 重新加载入口，抑制空态文案。
      if (_unsupported) {
        return RefreshIndicator(
          onRefresh: _reload,
          child: ListView(
            physics: const AlwaysScrollableScrollPhysics(),
            children: [
              const SizedBox(height: 24),
              _unsupportedBanner(),
              Padding(
                padding: const EdgeInsets.only(top: 8),
                child: Center(
                  child: TextButton(
                    key: const Key('attention-unsupported-reload'),
                    onPressed: _reload,
                    child: const Text('重新加载'),
                  ),
                ),
              ),
            ],
          ),
        );
      }
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
    final groups = groupAttentionItems(visible);
    return RefreshIndicator(
      onRefresh: _reload,
      child: ListView(
        physics: const AlwaysScrollableScrollPhysics(),
        padding: const EdgeInsets.symmetric(vertical: 4),
        children: [
          if (_unsupported) _unsupportedBanner(),
          if (_stale) _staleBanner(),
          // 头部操作行：刷新 + 全部已读（对齐 web 面板头部 reload / markAllRead）。
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: 4),
            child: Row(
              mainAxisAlignment: MainAxisAlignment.end,
              children: [
                TextButton(
                  key: const Key('attention-refresh'),
                  onPressed: _refreshing ? null : _reload,
                  child: Text(_refreshing ? '刷新中…' : '刷新'),
                ),
                // 禁用口径对齐 web：刷新/标记进行中或有单条标记在途时都不可点。
                TextButton(
                  key: const Key('attention-mark-all-read'),
                  onPressed: _markAllReadEnabled ? _markAllRead : null,
                  child: Text(_markAllBusy ? '标记中…' : '全部已读'),
                ),
              ],
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
            // 对齐 web zh dayFilter.todayEmpty 文案（无句号）。
            const Padding(
              padding: EdgeInsets.all(24),
              child: Center(child: Text('今天没有待处理事项')),
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

  /// 「全部已读」可用性：无未读、刷新中、全部标记进行中或存在单条标记在途时禁用
  /// （对齐 web MobileAttentionPanel disabled = loading||refreshing||pendingReadIds
  /// .size>0||unread===0）。
  bool get _markAllReadEnabled =>
      !_markAllBusy &&
      !_refreshing &&
      _pendingIds.isEmpty &&
      countUnreadAttentionItems(_items) > 0;

  /// Business Logic: 后端缺 attention.v1 时用户必须知道 Inbox 为何不可用，而不是
  /// 看到普通报错或空白（对齐 web attention:unsupported 横幅）。
  /// Code Logic: errorContainer 语义色横幅 + liveRegion，固定中文文案。
  Widget _unsupportedBanner() {
    final theme = Theme.of(context);
    return Padding(
      padding: const EdgeInsets.fromLTRB(8, 4, 8, 0),
      child: Material(
        key: const Key('attention-unsupported-banner'),
        color: theme.colorScheme.errorContainer,
        borderRadius: BorderRadius.circular(8),
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
          child: Semantics(
            liveRegion: true,
            child: Text(
              attentionUnsupportedMessage,
              style: theme.textTheme.bodySmall
                  ?.copyWith(color: theme.colorScheme.onErrorContainer),
            ),
          ),
        ),
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
                      // 来源动作用可见中文文案，未知 sourceKind 回退原值。
                      attentionSourceKindActionLabel(item.sourceKind),
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
