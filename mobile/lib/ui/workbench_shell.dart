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
    this.badges = const {},
  });

  final WorkbenchNavMode mode;
  final WorkbenchPanel panel;
  final ValueChanged<WorkbenchPanel> onSelect;
  final Widget child;
  final String? projectLabel;
  final String? subtitle;
  final Widget? worktreeStrip;
  final VoidCallback? onBackToProjects;

  /// 面板徽章计数（如「待处理」未读数）；null 或 <=0 不显示。
  final Map<WorkbenchPanel, int> badges;

  @override
  Widget build(BuildContext context) {
    final groups = getWorkbenchNavGroups(mode);
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
                    group.id,
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
            ],
          ),
        ),
      ),
      body: Column(
        children: [
          if (worktreeStrip != null)
            KeyedSubtree(key: const Key('worktree-strip'), child: worktreeStrip!),
          Expanded(child: child),
        ],
      ),
    );
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
