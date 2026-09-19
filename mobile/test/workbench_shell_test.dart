import 'package:cc_partner_mobile/workbench/nav.dart';
import 'package:cc_partner_mobile/workbench/shell.dart';
import 'package:test/test.dart';

void main() {
  test('dual-mode panels match /mobile and do not embed a WebView shell', () {
    expect(kEmbedsMobileSpa, isFalse);
    expect(isWorkbenchPanel('automation'), isTrue);
    expect(isWorkbenchPanel('browser'), isTrue);
    expect(isWorkbenchPanel('notes'), isFalse);
    expect(kDeferredPanels, equals(['notes']));
    expect(
      kWorkbenchPanels.map((p) => p.name),
      containsAll([
        'projects',
        'attention',
        'transfer',
        'settings',
        'provider',
        'terminal',
        'files',
        'git',
        'worktrees',
        'automation',
        'browser',
      ]),
    );
  });

  test('global nav is 项目 / 待处理 / 传输 / 设置 / Provider', () {
    final groups = getWorkbenchNavGroups(WorkbenchNavMode.global);
    final panels = groups.expand((g) => g.panels).toList();
    expect(panels, [
      WorkbenchPanel.projects,
      WorkbenchPanel.attention,
      WorkbenchPanel.transfer,
      WorkbenchPanel.settings,
      WorkbenchPanel.provider,
    ]);
    expect(panels.map(panelLabel).toList(), [
      '项目',
      '待处理',
      '传输',
      '设置',
      'Provider',
    ]);
  });

  test('project nav is 终端 / 浏览器 / 文件 / Git / worktrees / 自动化 plus shortcuts', () {
    final groups = getWorkbenchNavGroups(WorkbenchNavMode.project);
    expect(groups.map((g) => g.id).toList(), ['work', 'shortcuts']);
    expect(groups.first.panels, [
      WorkbenchPanel.terminal,
      WorkbenchPanel.browser,
      WorkbenchPanel.files,
      WorkbenchPanel.git,
      WorkbenchPanel.worktrees,
      WorkbenchPanel.automation,
    ]);
    expect(groups.first.panels.map(panelLabel).toList(), [
      '终端',
      '浏览器',
      '文件',
      'Git',
      'worktrees',
      '自动化',
    ]);
    expect(groups.last.panels, [
      WorkbenchPanel.attention,
      WorkbenchPanel.transfer,
      WorkbenchPanel.settings,
    ]);
  });

  test('resolveNavMode matches /mobile dual-mode rules', () {
    expect(resolveNavMode(WorkbenchPanel.terminal, false), WorkbenchNavMode.global);
    expect(resolveNavMode(WorkbenchPanel.projects, true), WorkbenchNavMode.global);
    expect(resolveNavMode(WorkbenchPanel.provider, true), WorkbenchNavMode.global);
    expect(resolveNavMode(WorkbenchPanel.terminal, true), WorkbenchNavMode.project);
    expect(resolveNavMode(WorkbenchPanel.attention, true), WorkbenchNavMode.project);
  });

  test('project-bound panels bounce to projects when no project is open', () {
    expect(
      selectPanelForProject(hasProject: false, next: WorkbenchPanel.automation),
      WorkbenchPanel.projects,
    );
    expect(
      selectPanelForProject(hasProject: true, next: WorkbenchPanel.automation),
      WorkbenchPanel.automation,
    );
  });

  test('worktree strip is on files / browser / git', () {
    expect(shouldShowWorktreeStrip(WorkbenchPanel.files), isTrue);
    expect(shouldShowWorktreeStrip(WorkbenchPanel.browser), isTrue);
    expect(shouldShowWorktreeStrip(WorkbenchPanel.git), isTrue);
    expect(shouldShowWorktreeStrip(WorkbenchPanel.terminal), isFalse);
    expect(shouldShowWorktreeStrip(WorkbenchPanel.projects), isFalse);
  });
}
