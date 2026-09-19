import 'package:cc_partner_mobile/address_book/models.dart';
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

  test('withBadges attaches counts without changing panel order', () {
    final items = withBadges(
      [WorkbenchPanel.projects, WorkbenchPanel.attention, WorkbenchPanel.transfer],
      {WorkbenchPanel.attention: 3},
    );
    expect(items.map((i) => i.panel).toList(), [
      WorkbenchPanel.projects,
      WorkbenchPanel.attention,
      WorkbenchPanel.transfer,
    ]);
    expect(items[0].badge, isNull);
    expect(items[1].badge, 3);
    expect(items[2].badge, isNull);
  });

  test('badge label hides zero/null and caps at 99+', () {
    expect(workbenchBadgeLabel(null), isNull);
    expect(workbenchBadgeLabel(0), isNull);
    expect(workbenchBadgeLabel(1), '1');
    expect(workbenchBadgeLabel(99), '99');
    expect(workbenchBadgeLabel(120), '99+');
    expect(workbenchBadgeLabel(-1), isNull);
  });

  test('restore resolves nothing without lastLocation or unknown project', () {
    expect(
      resolveWorkbenchLocationRestore(
        location: null,
        recentProjectIds: {'p1'},
      ),
      isNull,
    );
    expect(
      resolveWorkbenchLocationRestore(
        location: const LastLocation(projectId: 'p1'),
        recentProjectIds: {'p2'},
      ),
      isNull,
    );
    expect(
      resolveWorkbenchLocationRestore(
        location: const LastLocation(),
        recentProjectIds: {'p1'},
      ),
      isNull,
    );
  });

  test('restore falls back panel-first when panel name is invalid', () {
    final restore = resolveWorkbenchLocationRestore(
      location: const LastLocation(
        projectId: 'p1',
        panel: 'not-a-panel',
        worktreeId: 'wt-9',
        sessionId: 'tmux-9',
      ),
      recentProjectIds: {'p1'},
    );
    expect(restore, isNotNull);
    expect(restore!.projectId, 'p1');
    expect(restore.panel, WorkbenchPanel.terminal);
    expect(restore.worktreeId, 'wt-9');
    expect(restore.sessionId, 'tmux-9');
  });

  test('restore keeps a valid project-bound panel', () {
    final restore = resolveWorkbenchLocationRestore(
      location: const LastLocation(
        projectId: 'p1',
        panel: 'automation',
        worktreeId: 'wt-1',
        sessionId: 'tmux-1',
      ),
      recentProjectIds: {'p1', 'p2'},
    );
    expect(restore, isNotNull);
    expect(restore!.panel, WorkbenchPanel.automation);
    expect(restore.worktreeId, 'wt-1');
    expect(restore.sessionId, 'tmux-1');
  });
}
