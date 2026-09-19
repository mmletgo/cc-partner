import '../address_book/models.dart';

/// Dual-mode workbench panels matching `/mobile`. Not a WebView shell.
enum WorkbenchPanel {
  projects,
  attention,
  transfer,
  settings,
  provider,
  terminal,
  browser,
  files,
  git,
  worktrees,
  automation,
}

enum WorkbenchNavMode { global, project }

class WorkbenchNavGroup {
  const WorkbenchNavGroup({required this.id, required this.panels});
  final String id;
  final List<WorkbenchPanel> panels;
}

const kWorkbenchPanels = WorkbenchPanel.values;

const kProjectBoundPanels = <WorkbenchPanel>[
  WorkbenchPanel.terminal,
  WorkbenchPanel.browser,
  WorkbenchPanel.files,
  WorkbenchPanel.git,
  WorkbenchPanel.worktrees,
  WorkbenchPanel.automation,
];

const kGlobalNavGroups = <WorkbenchNavGroup>[
  WorkbenchNavGroup(id: 'projects', panels: [WorkbenchPanel.projects]),
  WorkbenchNavGroup(id: 'inbox', panels: [WorkbenchPanel.attention]),
  WorkbenchNavGroup(id: 'tools', panels: [WorkbenchPanel.transfer]),
  WorkbenchNavGroup(
    id: 'system',
    panels: [WorkbenchPanel.settings, WorkbenchPanel.provider],
  ),
];

/// 实验开关可以关闭的项目级面板集合（对齐 web ExperimentalFeaturesProvider 只滤 automation/browser）。
const kExperimentalPanels = <WorkbenchPanel>[
  WorkbenchPanel.automation,
  WorkbenchPanel.browser,
];

/// Business Logic: Drawer 分组标题此前直接渲染英文 id（projects/inbox/...），
/// 中文用户需要可读的分组标题（对齐 web navGroups 文案，inbox 用「待处理」口径）。
/// Code Logic: 纯函数映射；未知 id 回退原 id，保证旧数据/新分组不炸 UI。
String workbenchNavGroupLabel(String id) {
  switch (id) {
    case 'projects':
      return '项目';
    case 'inbox':
      return '待处理';
    case 'tools':
      return '工具';
    case 'system':
      return '系统';
    case 'work':
      return '工作';
    case 'shortcuts':
      return '快捷';
    default:
      return id;
  }
}

/// Business Logic: automation/browser 是内测开关，关闭时 Drawer 不能露出对应入口
/// （对齐 web 侧栏按 experimentalFeatures 过滤）。
/// Code Logic: 纯函数——按开关过滤 kExperimentalPanels，组内空了则整组移除；未知开关默认开。
List<WorkbenchNavGroup> filterWorkbenchNavGroupsByFeatures(
  List<WorkbenchNavGroup> groups, {
  required bool automationEnabled,
  required bool browserEnabled,
}) {
  bool enabled(WorkbenchPanel panel) {
    if (panel == WorkbenchPanel.automation) {
      return automationEnabled;
    }
    if (panel == WorkbenchPanel.browser) {
      return browserEnabled;
    }
    return true;
  }

  return [
    for (final group in groups)
      () {
        final panels = group.panels.where(enabled).toList();
        return WorkbenchNavGroup(id: group.id, panels: panels);
      }(),
  ].where((group) => group.panels.isNotEmpty).toList();
}

/// Business Logic: 用户停在 automation/browser 面板时开关被关闭（或加载前 fail-closed），
/// 必须自动回落到可用面板，不能留在黑屏页（对齐 web MobileWorkbench 的收回 effect）。
/// Code Logic: 纯函数——当前面板被关闭时回落：有项目 → terminal，无项目 → projects；否则原样返回。
WorkbenchPanel resolvePanelForFeatures({
  required WorkbenchPanel panel,
  required bool hasProject,
  required bool automationEnabled,
  required bool browserEnabled,
}) {
  final closed = (panel == WorkbenchPanel.automation && !automationEnabled) ||
      (panel == WorkbenchPanel.browser && !browserEnabled);
  if (!closed) {
    return panel;
  }
  return hasProject ? WorkbenchPanel.terminal : WorkbenchPanel.projects;
}

const kProjectNavGroups = <WorkbenchNavGroup>[
  WorkbenchNavGroup(id: 'work', panels: kProjectBoundPanels),
  WorkbenchNavGroup(
    id: 'shortcuts',
    panels: [
      WorkbenchPanel.attention,
      WorkbenchPanel.transfer,
      WorkbenchPanel.settings,
    ],
  ),
];

bool isWorkbenchPanel(String panel) =>
    WorkbenchPanel.values.any((item) => item.name == panel);

WorkbenchPanel? parseWorkbenchPanel(String? name) {
  if (name == null) {
    return null;
  }
  for (final panel in WorkbenchPanel.values) {
    if (panel.name == name) {
      return panel;
    }
  }
  return null;
}

bool isProjectBoundPanel(WorkbenchPanel panel) =>
    kProjectBoundPanels.contains(panel);

List<WorkbenchNavGroup> getWorkbenchNavGroups(WorkbenchNavMode mode) =>
    mode == WorkbenchNavMode.project ? kProjectNavGroups : kGlobalNavGroups;

WorkbenchNavMode resolveNavMode(WorkbenchPanel panel, bool hasActiveProject) {
  if (!hasActiveProject) {
    return WorkbenchNavMode.global;
  }
  if (panel == WorkbenchPanel.projects || panel == WorkbenchPanel.provider) {
    return WorkbenchNavMode.global;
  }
  return WorkbenchNavMode.project;
}

WorkbenchPanel selectPanelForProject({
  required bool hasProject,
  required WorkbenchPanel next,
}) {
  if (isProjectBoundPanel(next) && !hasProject) {
    return WorkbenchPanel.projects;
  }
  return next;
}

/// Business Logic: 终端面板也要显示 worktree 切换条（对齐 web 终端页 tabs），
/// 全屏时由 shell 用 hideWorktreeStrip 隐藏，而不是在这里排除终端。
/// Code Logic: 纯函数——files/browser/git/terminal 显示，其余面板不显示。
bool shouldShowWorktreeStrip(WorkbenchPanel panel) =>
    panel == WorkbenchPanel.files ||
    panel == WorkbenchPanel.browser ||
    panel == WorkbenchPanel.git ||
    panel == WorkbenchPanel.terminal;

/// 带徽章的导航项（如「待处理」的未读数）。
class WorkbenchNavItem {
  const WorkbenchNavItem({required this.panel, this.badge});

  final WorkbenchPanel panel;

  /// 徽章计数；null 或 <=0 不显示。
  final int? badge;
}

/// Business Logic: Drawer 需要在不改变导航分组的前提下挂未读徽章。
/// Code Logic: 按面板查找徽章计数套到导航项上，保持原有顺序。
List<WorkbenchNavItem> withBadges(
  List<WorkbenchPanel> panels,
  Map<WorkbenchPanel, int> badges,
) {
  return [
    for (final panel in panels) WorkbenchNavItem(panel: panel, badge: badges[panel]),
  ];
}

/// Business Logic: 徽章空间有限，超大数字需要截断展示。
/// Code Logic: null/<=0 返回 null（不显示），>99 显示 `99+`。
String? workbenchBadgeLabel(int? badge) {
  if (badge == null || badge <= 0) {
    return null;
  }
  return badge > 99 ? '99+' : '$badge';
}

/// 可以恢复的上次工作位置（面板已回落校验）。
class WorkbenchLocationRestore {
  const WorkbenchLocationRestore({
    required this.projectId,
    required this.panel,
    this.worktreeId,
    this.sessionId,
  });

  final String projectId;
  final WorkbenchPanel panel;
  final String? worktreeId;
  final String? sessionId;
}

/// Business Logic: 再次进入某 PC 工作台时应恢复上次的项目/面板/worktree/session；
/// 项目已不在最近列表或无记录则放弃恢复（保持现状行为）。
/// Code Logic: 纯函数回落——projectId 不在 recentProjectIds 返回 null；
/// 面板名无效回落终端；session/worktree 有效性由调用方按列表继续校验
/// （session→worktree→面板默认，TerminalPage/resolveActiveWorktreeId 已有回落）。
WorkbenchLocationRestore? resolveWorkbenchLocationRestore({
  required LastLocation? location,
  required Set<String> recentProjectIds,
}) {
  final loc = location;
  final projectId = loc?.projectId;
  if (loc == null || projectId == null || projectId.isEmpty) {
    return null;
  }
  if (!recentProjectIds.contains(projectId)) {
    return null;
  }
  return WorkbenchLocationRestore(
    projectId: projectId,
    panel: parseWorkbenchPanel(loc.panel) ?? WorkbenchPanel.terminal,
    worktreeId: loc.worktreeId,
    sessionId: loc.sessionId,
  );
}

String panelLabel(WorkbenchPanel panel) {
  switch (panel) {
    case WorkbenchPanel.projects:
      return '项目';
    case WorkbenchPanel.attention:
      return '待处理';
    case WorkbenchPanel.transfer:
      return '传输';
    case WorkbenchPanel.settings:
      return '设置';
    case WorkbenchPanel.provider:
      return 'Provider';
    case WorkbenchPanel.terminal:
      return '终端';
    case WorkbenchPanel.browser:
      return '浏览器';
    case WorkbenchPanel.files:
      return '文件';
    case WorkbenchPanel.git:
      return 'Git';
    case WorkbenchPanel.worktrees:
      return 'worktrees';
    case WorkbenchPanel.automation:
      return '自动化';
  }
}

IconDataForPanel iconForPanel(WorkbenchPanel panel) {
  switch (panel) {
    case WorkbenchPanel.projects:
      return IconDataForPanel.folder;
    case WorkbenchPanel.attention:
      return IconDataForPanel.inbox;
    case WorkbenchPanel.transfer:
      return IconDataForPanel.swap;
    case WorkbenchPanel.settings:
      return IconDataForPanel.settings;
    case WorkbenchPanel.provider:
      return IconDataForPanel.hub;
    case WorkbenchPanel.terminal:
      return IconDataForPanel.terminal;
    case WorkbenchPanel.browser:
      return IconDataForPanel.language;
    case WorkbenchPanel.files:
      return IconDataForPanel.file;
    case WorkbenchPanel.git:
      return IconDataForPanel.merge;
    case WorkbenchPanel.worktrees:
      return IconDataForPanel.accountTree;
    case WorkbenchPanel.automation:
      return IconDataForPanel.auto;
  }
}

enum IconDataForPanel {
  folder,
  inbox,
  swap,
  settings,
  hub,
  terminal,
  language,
  file,
  merge,
  accountTree,
  auto,
}
