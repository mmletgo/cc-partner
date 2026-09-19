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

bool shouldShowWorktreeStrip(WorkbenchPanel panel) =>
    panel == WorkbenchPanel.files ||
    panel == WorkbenchPanel.browser ||
    panel == WorkbenchPanel.git;

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
