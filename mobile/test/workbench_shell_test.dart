import 'package:cc_partner_mobile/address_book/models.dart';
import 'package:cc_partner_mobile/ui/workbench_shell.dart';
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

  test('project nav is 终端 / 预览 / 文件 / Git / Worktrees / 自动化 plus shortcuts', () {
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
      '预览',
      '文件',
      'Git',
      'Worktrees',
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

  test('worktree strip is on files / browser / git / terminal (fullscreen hides via shell)', () {
    expect(shouldShowWorktreeStrip(WorkbenchPanel.files), isTrue);
    expect(shouldShowWorktreeStrip(WorkbenchPanel.browser), isTrue);
    expect(shouldShowWorktreeStrip(WorkbenchPanel.git), isTrue);
    expect(shouldShowWorktreeStrip(WorkbenchPanel.terminal), isTrue);
    expect(shouldShowWorktreeStrip(WorkbenchPanel.projects), isFalse);
    expect(shouldShowWorktreeStrip(WorkbenchPanel.attention), isFalse);
  });

  test('nav group ids render Chinese labels aligned with web zh navGroups', () {
    expect(workbenchNavGroupLabel('projects'), '项目');
    expect(workbenchNavGroupLabel('inbox'), '收件箱');
    expect(workbenchNavGroupLabel('tools'), '工具');
    expect(workbenchNavGroupLabel('system'), '系统');
    expect(workbenchNavGroupLabel('work'), '工作台');
    expect(workbenchNavGroupLabel('shortcuts'), '快捷');
    expect(workbenchNavGroupLabel('unknown-group'), 'unknown-group');
  });

  test('experimental switches filter automation/browser out of nav groups', () {
    final all = getWorkbenchNavGroups(WorkbenchNavMode.project);
    expect(all.first.panels, containsAll([WorkbenchPanel.automation, WorkbenchPanel.browser]));

    final bothOff = filterWorkbenchNavGroupsByFeatures(
      all,
      automationEnabled: false,
      browserEnabled: false,
    );
    expect(bothOff.first.panels, isNot(contains(WorkbenchPanel.automation)));
    expect(bothOff.first.panels, isNot(contains(WorkbenchPanel.browser)));
    expect(bothOff.first.panels, contains(WorkbenchPanel.terminal));

    final browserOnly = filterWorkbenchNavGroupsByFeatures(
      all,
      automationEnabled: false,
      browserEnabled: true,
    );
    expect(browserOnly.first.panels, contains(WorkbenchPanel.browser));
    expect(browserOnly.first.panels, isNot(contains(WorkbenchPanel.automation)));
  });

  test('panels closed by experimental switches fall back to terminal/projects', () {
    expect(
      resolvePanelForFeatures(
        panel: WorkbenchPanel.automation,
        hasProject: true,
        automationEnabled: false,
        browserEnabled: true,
      ),
      WorkbenchPanel.terminal,
    );
    expect(
      resolvePanelForFeatures(
        panel: WorkbenchPanel.browser,
        hasProject: false,
        automationEnabled: true,
        browserEnabled: false,
      ),
      WorkbenchPanel.projects,
    );
    expect(
      resolvePanelForFeatures(
        panel: WorkbenchPanel.automation,
        hasProject: true,
        automationEnabled: true,
        browserEnabled: true,
      ),
      WorkbenchPanel.automation,
    );
    expect(
      resolvePanelForFeatures(
        panel: WorkbenchPanel.git,
        hasProject: true,
        automationEnabled: false,
        browserEnabled: false,
      ),
      WorkbenchPanel.git,
    );
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

  test('connection: success goes online, failure keeps cachedSince and error', () {
    final online = WorkbenchConnectionState.online(
      lastSucceededAt: DateTime(2026, 9, 20, 10, 30),
    );
    // 失败：保留上次成功时间做「缓存于」提示，并携带最近错误。
    final offline = markWorkbenchConnectionFailure('boom', online);
    expect(offline.kind, WorkbenchConnectionKind.offline);
    expect(offline.lastError, 'boom');
    expect(offline.cachedSince, DateTime(2026, 9, 20, 10, 30));

    // 连续失败保留更早的缓存起点；首次失败（prev 为 null）无缓存提示。
    final again = markWorkbenchConnectionFailure('boom2', offline);
    expect(again.cachedSince, DateTime(2026, 9, 20, 10, 30));
    expect(markWorkbenchConnectionFailure('first', null).cachedSince, isNull);
  });

  test('connection: recovery edge fires only from offline/reconnecting to online', () {
    final online = WorkbenchConnectionState.online(
      lastSucceededAt: DateTime(2026, 9, 20, 10, 30),
    );
    final offline = markWorkbenchConnectionFailure('boom', online);
    final recovered = WorkbenchConnectionState.online(
      lastSucceededAt: DateTime(2026, 9, 20, 11, 0),
    );
    expect(shouldRefreshWorkbenchOnReconnect(offline, recovered), isTrue);
    expect(shouldRefreshWorkbenchOnReconnect(null, recovered), isFalse,
        reason: '首次成功不是恢复边沿');
    expect(shouldRefreshWorkbenchOnReconnect(online, recovered), isFalse,
        reason: '已在线时再次成功不触发刷新');
    expect(
      shouldRefreshWorkbenchOnReconnect(
        online,
        markWorkbenchConnectionFailure('down', online),
      ),
      isFalse,
    );
  });

  test('connection: cached time and pill labels match web zh copy', () {
    final online = WorkbenchConnectionState.online(
      lastSucceededAt: DateTime(2026, 9, 20, 10, 30),
    );
    expect(workbenchConnectionCachedAt(online), isNull,
        reason: 'online 不显示缓存时间');
    expect(workbenchConnectionLabel(online), '已连接');

    final offline = markWorkbenchConnectionFailure('boom', online);
    expect(workbenchConnectionCachedAt(offline), DateTime(2026, 9, 20, 10, 30));
    expect(workbenchConnectionLabel(offline), '离线');
    expect(workbenchConnectionLabel(null), isNull);
    expect(
      workbenchConnectionLabel(
        WorkbenchConnectionState.reconnecting(cachedSince: online.lastSucceededAt),
      ),
      '重新连接中…',
    );
    expect(formatWorkbenchCachedTime(DateTime(2026, 1, 1, 9, 5)), '09:05');
  });
}
