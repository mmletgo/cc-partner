import 'package:flutter/material.dart';

import '../workbench/nav.dart';

/// 壳层连接态种类（对齐 web MobileConnectionState 的简易三态）。
enum WorkbenchConnectionKind { online, reconnecting, offline }

/// 壳层连接态：由「最近一次 worktrees/projects 请求成败」驱动（对齐 web
/// mobileWorkbenchState.ts 的 MobileConnectionState）。
///
/// Business Logic: 弱网/后端离线时用户需要知道当前数据来自缓存以及失败原因，
/// 恢复后要自动刷新当前面板的权威数据（恢复边沿见
/// [shouldRefreshWorkbenchOnReconnect]）。
///
/// Code Logic: 不可变值对象；offline 在 web 基础上额外保留 cachedSince，
/// 供「缓存于 HH:mm」提示（web 的 offline 无缓存时间，这里按移动端壳层需要保留）。
class WorkbenchConnectionState {
  const WorkbenchConnectionState.online({
    required DateTime this.lastSucceededAt,
  }) : kind = WorkbenchConnectionKind.online,
       cachedSince = null,
       lastError = null;

  const WorkbenchConnectionState.reconnecting({
    this.cachedSince,
    this.lastError,
  }) : kind = WorkbenchConnectionKind.reconnecting,
       lastSucceededAt = null;

  const WorkbenchConnectionState.offline({
    required this.lastError,
    this.cachedSince,
  }) : kind = WorkbenchConnectionKind.offline,
       lastSucceededAt = null;

  final WorkbenchConnectionKind kind;

  /// 最近一次成功时间（online 专有）。
  final DateTime? lastSucceededAt;

  /// 进入失败态前的最后成功时间（reconnecting/offline 用于「缓存于」提示）。
  final DateTime? cachedSince;

  /// 最近一次失败的错误文案（reconnecting/offline）。
  final String? lastError;
}

/// Business Logic: worktrees/projects 请求失败后壳层要进入离线态，
/// 同时保留上次成功时间做「缓存于」提示（对齐 web markMobileConnectionOffline，
/// 差异仅是额外保留 cachedSince 供展示）。
/// Code Logic: 纯函数——online 的 lastSucceededAt / 失败态的 cachedSince 透传为新的 cachedSince。
WorkbenchConnectionState markWorkbenchConnectionFailure(
  String lastError,
  WorkbenchConnectionState? prev,
) {
  final cachedSince = prev == null
      ? null
      : (prev.kind == WorkbenchConnectionKind.online
            ? prev.lastSucceededAt
            : prev.cachedSince);
  return WorkbenchConnectionState.offline(
    lastError: lastError,
    cachedSince: cachedSince,
  );
}

/// Business Logic: 从 offline/reconnecting 恢复 online 时要刷新当前面板权威数据
/// （对齐 web shouldRefreshMobilePanelOnReconnect）；首次成功（prev 为 null）不算恢复。
/// Code Logic: 纯函数——next 为 online 且 prev 存在且 prev 非 online 时返回 true。
bool shouldRefreshWorkbenchOnReconnect(
  WorkbenchConnectionState? prev,
  WorkbenchConnectionState next,
) {
  return next.kind == WorkbenchConnectionKind.online &&
      prev != null &&
      prev.kind != WorkbenchConnectionKind.online;
}

/// Business Logic: 状态行的「缓存于」提示只在非 online 且确有上次成功时间时展示
/// （对齐 web getMobileConnectionCachedAt + cachedLabel 条件）。
/// Code Logic: 纯函数——reconnecting/offline 返回 cachedSince，其余返回 null。
DateTime? workbenchConnectionCachedAt(WorkbenchConnectionState? connection) {
  final conn = connection;
  if (conn == null || conn.kind == WorkbenchConnectionKind.online) {
    return null;
  }
  return conn.cachedSince;
}

/// Business Logic: 连接态药丸文案逐字对齐 web zh workbench.mobile.connection。
/// Code Logic: 纯函数映射；connection 为 null（尚无成败记录）时不显示药丸。
String? workbenchConnectionLabel(WorkbenchConnectionState? connection) {
  final conn = connection;
  if (conn == null) {
    return null;
  }
  switch (conn.kind) {
    case WorkbenchConnectionKind.online:
      return '已连接';
    case WorkbenchConnectionKind.reconnecting:
      return '重新连接中…';
    case WorkbenchConnectionKind.offline:
      return '离线';
  }
}

/// Business Logic: 「缓存于 HH:mm」要求两位数时刻，避免 9:5 这类难读输出。
/// Code Logic: 纯函数—— HH:mm 补零格式化。
String formatWorkbenchCachedTime(DateTime time) {
  final hh = time.hour.toString().padLeft(2, '0');
  final mm = time.minute.toString().padLeft(2, '0');
  return '$hh:$mm';
}

IconData _icon(IconDataForPanel kind) {
  switch (kind) {
    case IconDataForPanel.folder:
      return Icons.folder_outlined;
    case IconDataForPanel.inbox:
      return Icons.inbox_outlined;
    case IconDataForPanel.swap:
      return Icons.swap_horiz;
    case IconDataForPanel.settings:
      return Icons.settings_outlined;
    case IconDataForPanel.hub:
      return Icons.hub_outlined;
    case IconDataForPanel.terminal:
      return Icons.terminal;
    case IconDataForPanel.language:
      return Icons.language;
    case IconDataForPanel.file:
      return Icons.insert_drive_file_outlined;
    case IconDataForPanel.merge:
      return Icons.merge_type;
    case IconDataForPanel.accountTree:
      return Icons.account_tree_outlined;
    case IconDataForPanel.auto:
      return Icons.auto_mode_outlined;
  }
}

/// Drawer 子树探针：DrawerController 在 dismissed 状态不构建子树，因此本 State
/// 每次抽屉打开都会重新 initState，作为「抽屉已打开」的一次性信号。
class _DrawerOpenedProbe extends StatefulWidget {
  const _DrawerOpenedProbe({required this.child, this.onOpened});

  final Widget child;

  /// 抽屉打开后的回调（post-frame 触发，回调内可安全 setState）。
  final VoidCallback? onOpened;

  @override
  State<_DrawerOpenedProbe> createState() => _DrawerOpenedProbeState();
}

class _DrawerOpenedProbeState extends State<_DrawerOpenedProbe> {
  @override
  void initState() {
    super.initState();
    final onOpened = widget.onOpened;
    if (onOpened != null) {
      // 等本帧结束再回调，避免在 build/布局期间触发宿主 setState。
      WidgetsBinding.instance.addPostFrameCallback((_) => onOpened());
    }
  }

  @override
  Widget build(BuildContext context) => widget.child;
}

/// Dual-mode workbench chrome matching `/mobile`. Never embeds the SPA.
class WorkbenchShell extends StatelessWidget {
  const WorkbenchShell({
    super.key,
    required this.mode,
    required this.panel,
    required this.onSelect,
    required this.child,
    this.projectLabel,
    this.subtitle,
    this.worktreeStrip,
    this.onBackToProjects,
    this.onDrawerOpened,
    this.badges = const {},
    this.automationEnabled = true,
    this.browserEnabled = true,
    this.hideWorktreeStrip = false,
    this.hideAppBar = false,
    this.connection,
    this.worktreeLabel,
    this.sessionLabel,
  });

  final WorkbenchNavMode mode;
  final WorkbenchPanel panel;
  final ValueChanged<WorkbenchPanel> onSelect;
  final Widget child;
  final String? projectLabel;
  final String? subtitle;
  final Widget? worktreeStrip;
  final VoidCallback? onBackToProjects;

  /// 抽屉每次打开时回调（experimentalFeatures 拉取失败后的静默重试钩子）。
  final VoidCallback? onDrawerOpened;

  /// 面板徽章计数（如「待处理」未读数）；null 或 <=0 不显示。
  final Map<WorkbenchPanel, int> badges;

  /// 内测开关：关闭时 Drawer 隐藏对应入口（fail-closed 对齐 web）。
  final bool automationEnabled;
  final bool browserEnabled;

  /// 终端全屏等工作区场景下隐藏 worktree 切换条。
  final bool hideWorktreeStrip;

  /// 终端全屏时隐藏 AppBar 与状态行（对齐 web 全屏 fixed overlay 盖住整个 shell）。
  final bool hideAppBar;

  /// 壳层连接态（null = 尚无成败记录，不显示连接药丸，对齐 web 初始 null）。
  final WorkbenchConnectionState? connection;

  /// 当前 worktree 显示名；null 时状态行回落「worktree」占位（对齐 web status.worktree）。
  final String? worktreeLabel;

  /// 当前会话名；null 时状态行回落「session」占位（对齐 web status.session）。
  final String? sessionLabel;

  @override
  Widget build(BuildContext context) {
    final groups = filterWorkbenchNavGroupsByFeatures(
      getWorkbenchNavGroups(mode),
      automationEnabled: automationEnabled,
      browserEnabled: browserEnabled,
    );
    final showAppBar = !hideAppBar;
    final body = Column(
      children: [
        if (showAppBar) _statusSection(context),
        if (worktreeStrip != null && !hideWorktreeStrip)
          KeyedSubtree(key: const Key('worktree-strip'), child: worktreeStrip!),
        Expanded(child: child),
      ],
    );
    return Scaffold(
      key: const Key('workbench-shell'),
      appBar: showAppBar
          ? AppBar(
              title: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text(
                    projectLabel ?? panelLabel(panel),
                    overflow: TextOverflow.ellipsis,
                  ),
                  if (subtitle != null)
                    Text(
                      subtitle!,
                      style: Theme.of(context).textTheme.bodySmall,
                    ),
                ],
              ),
            )
          : null,
      // body 的父级类型必须保持不变。全屏时如果从 Column 换成 SafeArea，
      // 终端页会被拆掉重建，dispose 里又把全屏关掉，看起来就像按钮没反应。
      body: SafeArea(
        top: !showAppBar,
        bottom: !showAppBar,
        maintainBottomViewPadding: !showAppBar,
        child: body,
      ),
      drawer: Drawer(
        child: SafeArea(
          child: _DrawerOpenedProbe(
            onOpened: onDrawerOpened,
            child: ListView(
              children: [
                if (mode == WorkbenchNavMode.project &&
                    onBackToProjects != null)
                  ListTile(
                    key: const Key('nav-back-projects'),
                    leading: const Icon(Icons.arrow_back),
                    title: const Text('项目'),
                    onTap: () {
                      Navigator.of(context).pop();
                      onBackToProjects!();
                    },
                  ),
                for (final group in groups) ...[
                  Padding(
                    padding: const EdgeInsets.fromLTRB(16, 12, 16, 4),
                    child: Text(
                      workbenchNavGroupLabel(group.id),
                      style: Theme.of(context).textTheme.labelSmall,
                    ),
                  ),
                  for (final item in withBadges(group.panels, badges))
                    ListTile(
                      key: Key('nav-${item.panel.name}'),
                      selected: item.panel == panel,
                      leading: Icon(_icon(iconForPanel(item.panel))),
                      title: Text(panelLabel(item.panel)),
                      trailing: _badge(context, item),
                      onTap: () {
                        Navigator.of(context).pop();
                        onSelect(item.panel);
                      },
                    ),
                ],
                // 全局模式组尾放「断开并返回地址簿」：项目模式不放，避免操作中误触。
                if (mode == WorkbenchNavMode.global)
                  ListTile(
                    key: const Key('nav-disconnect'),
                    leading: const Icon(Icons.link_off),
                    title: const Text('断开并返回地址簿'),
                    onTap: () => _disconnect(context),
                  ),
              ],
            ),
          ),
        ),
      ),
    );
  }

  /// Business Logic: 壳层要有一条只读状态行（对齐 web MobileWorkbenchShell 的
  /// statusRow）：worktree 显示名 · 会话名 · 连接态药丸（含「缓存于 HH:mm」），
  /// 离线时在下方整行展示最近错误（对齐 web offlineError 行）。
  /// Code Logic: Wrap 布局防长名溢出；连接态为 null 时不出连接药丸；
  /// 错误行仅在 offline 且有 lastError 时渲染。
  Widget _statusSection(BuildContext context) {
    final connection = this.connection;
    final connectionText = workbenchConnectionLabel(connection);
    final cachedAt = workbenchConnectionCachedAt(connection);
    final pillText = connectionText == null
        ? null
        : (cachedAt == null
              ? connectionText
              : '$connectionText · 缓存于 ${formatWorkbenchCachedTime(cachedAt)}');
    final theme = Theme.of(context);
    Widget? errorLine;
    if (connection != null &&
        connection.kind == WorkbenchConnectionKind.offline &&
        (connection.lastError ?? '').isNotEmpty) {
      errorLine = Padding(
        key: const Key('shell-status-error'),
        padding: const EdgeInsets.fromLTRB(12, 0, 12, 4),
        child: Text(
          '最后错误：${connection.lastError}',
          style: theme.textTheme.bodySmall?.copyWith(
            color: theme.colorScheme.error,
          ),
          overflow: TextOverflow.ellipsis,
        ),
      );
    }
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      mainAxisSize: MainAxisSize.min,
      children: [
        Padding(
          padding: EdgeInsets.fromLTRB(12, 6, 12, errorLine == null ? 6 : 2),
          child: Wrap(
            spacing: 6,
            runSpacing: 4,
            children: [
              _statusPill(
                context,
                (worktreeLabel == null || worktreeLabel!.isEmpty)
                    ? 'worktree'
                    : worktreeLabel!,
                key: const Key('shell-status-worktree'),
              ),
              _statusPill(
                context,
                (sessionLabel == null || sessionLabel!.isEmpty)
                    ? 'session'
                    : sessionLabel!,
                key: const Key('shell-status-session'),
              ),
              if (pillText != null)
                _statusPill(
                  context,
                  pillText,
                  key: const Key('shell-status-connection'),
                ),
            ],
          ),
        ),
        if (errorLine != null) errorLine,
      ],
    );
  }

  /// Business Logic: 状态行药丸是只读徽标，需要弱化视觉但与主题色联动。
  /// Code Logic: 圆角容器 + labelSmall；不用硬编码颜色，全部走主题 scheme。
  Widget _statusPill(BuildContext context, String text, {Key? key}) {
    final theme = Theme.of(context);
    return Container(
      key: key,
      padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 3),
      decoration: BoxDecoration(
        color: theme.colorScheme.surfaceContainerHighest,
        borderRadius: BorderRadius.circular(999),
      ),
      child: Text(
        text,
        style: theme.textTheme.labelSmall,
        overflow: TextOverflow.ellipsis,
      ),
    );
  }

  /// Business Logic: 全局模式 Drawer 组尾的「断开并返回地址簿」要把用户送回上一页
  /// （通常是地址簿），且必须走既有路由 pop 路径，让宿主页 PopScope 照常保存
  /// lastLocation，不得绕过。
  /// Code Logic: 第一次 pop 关闭 Drawer（DrawerController 打开时压入的
  /// LocalHistoryEntry），第二次 pop 退出工作台路由本身。
  void _disconnect(BuildContext context) {
    final navigator = Navigator.of(context);
    navigator.pop();
    navigator.pop();
  }

  /// Business Logic: 「待处理」未读数要在 Drawer 里一眼可见，0 不能显示空徽章。
  /// Code Logic: 有计数时渲染主题色圆角胶囊；无计数返回 null（ListTile 不占 trailing 位）。
  Widget? _badge(BuildContext context, WorkbenchNavItem item) {
    final label = workbenchBadgeLabel(item.badge);
    if (label == null) {
      return null;
    }
    final theme = Theme.of(context);
    return Container(
      key: Key('nav-badge-${item.panel.name}'),
      padding: const EdgeInsets.symmetric(horizontal: 7, vertical: 2),
      decoration: BoxDecoration(
        color: theme.colorScheme.primary,
        borderRadius: BorderRadius.circular(999),
      ),
      child: Text(
        label,
        style: theme.textTheme.labelSmall?.copyWith(
          color: theme.colorScheme.onPrimary,
        ),
      ),
    );
  }
}
