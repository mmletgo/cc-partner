import 'package:flutter/material.dart';

import '../workbench/nav.dart';

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

  @override
  Widget build(BuildContext context) {
    final groups = filterWorkbenchNavGroupsByFeatures(
      getWorkbenchNavGroups(mode),
      automationEnabled: automationEnabled,
      browserEnabled: browserEnabled,
    );
    return Scaffold(
      key: const Key('workbench-shell'),
      appBar: AppBar(
        title: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(
              projectLabel ?? panelLabel(panel),
              overflow: TextOverflow.ellipsis,
            ),
            if (subtitle != null)
              Text(subtitle!, style: Theme.of(context).textTheme.bodySmall),
          ],
        ),
      ),
      drawer: Drawer(
        child: SafeArea(
          child: _DrawerOpenedProbe(
            onOpened: onDrawerOpened,
            child: ListView(
              children: [
                if (mode == WorkbenchNavMode.project && onBackToProjects != null)
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
      body: Column(
        children: [
          if (worktreeStrip != null && !hideWorktreeStrip)
            KeyedSubtree(key: const Key('worktree-strip'), child: worktreeStrip!),
          Expanded(child: child),
        ],
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
        style: theme.textTheme.labelSmall?.copyWith(color: theme.colorScheme.onPrimary),
      ),
    );
  }
}
