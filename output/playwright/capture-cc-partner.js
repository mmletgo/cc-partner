async (page) => {
  const timestamp = '2026-08-21T12:00:00.000Z';
  const project = {
    id: 'demo-project',
    name: 'cc-partner-demo',
    kind: 'local',
    deviceId: 'demo-mac',
    deviceName: 'Demo Mac',
    path: '/workspace/cc-partner-demo',
    lastOpenedAt: timestamp,
    createdAt: timestamp,
    updatedAt: timestamp,
  };
  const worktrees = [
    {
      id: 'demo-project:main', projectId: project.id, name: 'main', branch: 'main',
      baseBranch: null, path: project.path, isMain: true,
      status: { branch: 'main', changed: 0, ahead: 0, behind: 0, conflicts: 0, clean: true, canPush: false },
      createdAt: timestamp, updatedAt: timestamp,
    },
    {
      id: 'demo-worktree-feature', projectId: project.id, name: 'mobile-control', branch: 'feature/mobile-control',
      baseBranch: 'main', path: '/workspace/cc-partner-demo/.worktrees/mobile-control', isMain: false,
      status: { branch: 'feature/mobile-control', changed: 3, ahead: 2, behind: 0, conflicts: 0, clean: false, canPush: true },
      createdAt: timestamp, updatedAt: timestamp,
    },
  ];
  const sessions = [
    {
      id: 'session-main', projectId: project.id, worktreeId: null, name: 'Coding Agent · implementation',
      command: '/bin/zsh', cwd: project.path, status: 'running', cols: 110, rows: 30,
      startedAt: timestamp, exitedAt: null, exitCode: null, supportsPanes: true, paneCount: 2,
    },
    {
      id: 'session-tests', projectId: project.id, worktreeId: null, name: 'Tests & evidence',
      command: '/bin/zsh', cwd: project.path, status: 'running', cols: 110, rows: 30,
      startedAt: timestamp, exitedAt: null, exitCode: null, supportsPanes: true, paneCount: 1,
    },
  ];
  const files = [
    { name: 'web', path: 'web', kind: 'directory', size: null, modifiedAt: timestamp, children: null },
    { name: 'src-tauri', path: 'src-tauri', kind: 'directory', size: null, modifiedAt: timestamp, children: null },
    { name: 'docs', path: 'docs', kind: 'directory', size: null, modifiedAt: timestamp, children: null },
    { name: 'README.md', path: 'README.md', kind: 'file', size: 8421, modifiedAt: timestamp, children: null },
    { name: 'WORKFLOW.md', path: 'WORKFLOW.md', kind: 'file', size: 1290, modifiedAt: timestamp, children: null },
  ];
  const commits = [
    { hash: 'a1b2c3d4', shortHash: 'a1b2c3d', parentHashes: ['b2c3d4e5', 'c3d4e5f6'], authorName: 'Demo User', authorEmail: '', authoredAt: timestamp, summary: 'Merge mobile workbench controls', refs: [{ name: 'main', fullName: 'refs/heads/main', kind: 'local', remote: null, isHead: true }] },
    { hash: 'b2c3d4e5', shortHash: 'b2c3d4e', parentHashes: ['d4e5f6a7'], authorName: 'Demo User', authorEmail: '', authoredAt: timestamp, summary: 'Add validation evidence panel', refs: [] },
    { hash: 'c3d4e5f6', shortHash: 'c3d4e5f', parentHashes: ['d4e5f6a7'], authorName: 'Demo User', authorEmail: '', authoredAt: timestamp, summary: 'Polish mobile file workspace', refs: [{ name: 'feature/mobile-control', fullName: 'refs/heads/feature/mobile-control', kind: 'local', remote: null, isHead: false }] },
    { hash: 'd4e5f6a7', shortHash: 'd4e5f6a', parentHashes: [], authorName: 'Demo User', authorEmail: '', authoredAt: timestamp, summary: 'Create deterministic project workflow', refs: [] },
  ];
  const replay = {
    sessionId: 'session-main',
    buffer: [
      '\u001b[38;5;245mcc-partner-demo  main\u001b[0m',
      '$ coding-agent --goal "完善 Mobile Workbench 文件接手流程"',
      '',
      '\u001b[38;5;214m●\u001b[0m 已读取项目约定与相关测试',
      '\u001b[38;5;214m●\u001b[0m 正在实现文件保存与 Git 状态联动',
      '\u001b[38;5;82m✓\u001b[0m npm test -- MobileWorkbench',
      '\u001b[38;5;82m✓\u001b[0m npm run build',
      '',
      '下一步：生成验证 evidence，等待人工复核',
      '',
    ].join('\r\n'),
    truncated: false,
    lastSeq: 32,
  };

  await page.addInitScript(({ project, worktrees, sessions, files, commits, replay, timestamp }) => {
    localStorage.setItem('cp-lang', 'zh');
    localStorage.setItem('cp-theme', 'light');
    localStorage.setItem('cp-permission-onboarded', '1');
    localStorage.setItem('cp-workbench-active-project-id', project.id);

    const callbacks = new Map();
    const eventCallbacks = new Map();
    let callbackId = 0;
    let eventId = 0;
    const unknown = [];
    window.__ccPartnerCaptureUnknown = unknown;

    const config = {
      deviceId: 'demo-mac', deviceName: 'Demo Mac', receiveDir: '/workspace/received', gamePluginDir: '/workspace/plugins',
      screenshotHotkey: 'CommandOrControl+Shift+S', promptOptimizerHotkey: 'Control', promptQuickInputHotkey: '<ctrl>+/',
      promptOptimizerFillLanguage: 'zh', promptOptimizerProvider: 'claude', httpPort: 62116,
      experimentalFeatures: { battery: false, game: false, browser: false, automation: true, cloudSync: false },
    };
    const emptyAttention = {
      generatedAt: timestamp,
      counts: { total: 0, decision: 0, blocked: 0, environment: 0, unreadTotal: 0, unreadDecision: 0, unreadBlocked: 0, unreadEnvironment: 0 },
      myDeviceId: 'demo-mac', items: [],
    };
    const dependency = { status: 'ready', available: true, version: '3.4', backend: 'native', path: '/usr/local/bin/tmux', installable: false, installCommandPreview: [], error: null, output: [], statusChangedAt: timestamp };
    const invoke = async (cmd, args = {}) => {
      if (cmd === 'plugin:event|listen') {
        eventId += 1;
        if (typeof args.handler === 'number') eventCallbacks.set(eventId, args.handler);
        return eventId;
      }
      if (cmd === 'plugin:event|unlisten') return undefined;
      if (cmd.startsWith('plugin:notification|')) return true;
      if (cmd === 'plugin:window|start_dragging' || cmd === 'plugin:window|set_focus') return null;
      switch (cmd) {
        case 'check_permissions': return { screenCapture: { granted: true }, accessibility: { granted: true }, inputMonitoring: { granted: true, state: 'granted' }, notification: { granted: true } };
        case 'get_version': return '0.9.0';
        case 'get_config': case 'get_default_config': return config;
        case 'list_attention_items': return emptyAttention;
        case 'list_workbench_projects': return [project];
        case 'list_workbench_worktrees': return worktrees;
        case 'list_workbench_sessions': return sessions;
        case 'list_workbench_window_occupancy': return [];
        case 'get_workbench_launch_summary': return { projects: { kind: 'ready', value: [project] }, sessions: { kind: 'ready', value: sessions }, tasks: { kind: 'ready', value: [] }, transfers: { kind: 'ready', value: [] }, devices: { kind: 'ready', value: [] }, generatedAt: timestamp };
        case 'check_workbench_dependency': case 'get_workbench_dependency_install_status': case 'get_workbench_dependency_status': return dependency;
        case 'list_github_trending_repos': return { repos: [], cached: true, generatedAt: null };
        case 'check_lan_firewall_dependency': return { platform: 'macos', available: true, status: 'ready', requiredTcpPort: 62116, requiredUdpPort: 5353, activeHttpPort: 62116, addresses: ['192.168.1.20'], guidance: [] };
        case 'get_mobile_access_info': return { deviceName: 'Demo Mac', port: 62116, urls: ['http://192.168.1.20:62116/mobile'], entries: [{ id: '192.168.1.20', url: 'http://192.168.1.20:62116/mobile', host: '192.168.1.20', role: 'wifi', isDefault: true }] };
        case 'get_lan_disclosure_status': return { acknowledged: true, version: 1, actualHttpPort: 62116, preferredHttpPort: 62116, localAddresses: ['192.168.1.20'], mdnsPort: 5353, started: true };
        case 'get_operational_notification_snapshot': return { asOfCursor: { ownerInstanceId: 'demo-owner', sequence: 0 }, items: [], truncated: false };
        case 'get_orchestrator_config': case 'get_default_orchestrator_config': return { enabled: true, maxConcurrentTasks: 2, verificationTimeoutSeconds: 900, runnerTimeoutSeconds: 3600, autoCommit: false, autoPushTaskBranch: false, autoMergeMain: false, autoPushMain: false, experimentsEnabled: false, maxConcurrentExperiments: 2 };
        case 'get_orchestrator_runtime_snapshot': return { projectId: project.id, schedulerEnabled: true, workflowSource: 'project', slots: { used: 1, total: 2 }, runningTasks: [], retryingTasks: [], recentEvents: [], generatedAt: timestamp };
        case 'list_orchestrator_tasks': return [];
        case 'list_orchestrator_remote_outbox': return [];
        case 'get_focused_workbench_session': return { sessionId: sessions[0].id };
        case 'focus_workbench_session': return { ok: true, sessionId: args.sessionId ?? sessions[0].id };
        case 'replay_workbench_session': return { ...replay, sessionId: args.sessionId ?? replay.sessionId };
        case 'list_workbench_dir': return files;
        case 'list_workbench_git_commits': return commits;
        case 'get_workbench_path_info': return files.find((file) => file.path === args.path) ?? files[3];
        case 'touch_workbench_project': return project;
        case 'open_workbench_file': return {
          metadata: files[3], detectedType: 'markdown',
          capabilities: { canPreview: true, canEdit: true, canFormat: false, mustValidateBeforeSave: false, defaultMode: 'edit', availableModes: ['edit', 'preview', 'source'] },
          text: { content: '# cc-partner demo\n\n本地优先的多设备 AI 项目工作台。\n\n- Worktree 隔离\n- 终端可见\n- 手机接手\n- 验证 evidence\n', baseHash: 'demo-hash', baseModifiedAt: timestamp },
          image: null, csv: null, sqlite: null, truncated: false, notice: null,
        };
        case 'get_agent_runtime_snapshot': return {
          ownerInstanceId: 'demo-owner', asOfSequence: 1, projectId: project.id, truncated: false,
          sessions: [{
            id: 'agent-demo-1', projectId: project.id, worktreeId: 'demo-project:main', terminalSessionId: 'session-main',
            orchestratorTaskId: null, orchestratorAttempt: null, providerId: 'demo-agent', phase: 'working', version: 1,
            startedAt: timestamp, lastActivityAt: timestamp, endedAt: null, outcomeCode: null,
            resumedFromAgentSessionId: null, isActive: true,
            usage: { modelId: 'coding-model', inputTokens: 68420, outputTokens: 12860, cacheReadTokens: 41200, cacheWriteTokens: 3200, contextLength: 81280, contextWindow: 1000000, activeDurationMs: 3960000, firstTokenAvgMs: 740, extractedAt: timestamp },
          }],
        };
        case 'get_workbench_lan_fleet': return { generatedAt: timestamp, devices: [] };
        case 'list_agent_ledger': return [];
        case 'summarize_agent_ledger': return { window: '7d', sessions: 0, inputTokens: null, outputTokens: null, totalTokens: null, durationMs: 0, byAgent: [] };
        case 'claim_workbench_window_project': return { action: 'claimed', label: 'main', projectId: project.id };
        case 'apply_workbench_window_deeplink': case 'focus_workbench_window': case 'close_workbench_window': return null;
        case 'get_app_identity': return { bundleId: 'com.cc-partner.app', flavor: 'release' };
        case 'get_workbench_banner': return { markdown: '把每一个 Agent 任务推进到可验证的交付。', updatedAt: timestamp };
        case 'get_workspace_layout': return null;
        case 'save_workspace_layout': return { schemaVersion: 1, id: 'layout-demo', revision: 1, createdAt: timestamp, updatedAt: timestamp, ...args.draft };
        case 'preflight_workspace_restore_cmd': return { restoreId: 'restore-empty', layoutId: '', layoutRevision: 0, status: 'empty', resolvedProjectId: null, resolvedWorktreeId: null, resolvedSessionId: null, workspaceView: 'terminal', inspectorTab: 'git', browserTargetUrl: null, actions: [] };
        case 'apply_workspace_restore_cmd': return { restoreId: 'restore-empty', status: 'empty', restoredCount: 0, skippedCount: 0, actions: [] };
        case 'discover_workbench_browser_targets': return { projectId: project.id, worktreeId: args.worktreeId ?? null, targets: [], selectedTargetId: null };
        case 'resize_workbench_session': return { ok: true, sessionId: args.sessionId };
        case 'get_battery_snapshot': return { mode: 'unlimited', balanceMinutes: 0, maxBalanceMinutes: 480, todayEarnedMinutes: 0, todaySpentMinutes: 0, updatedAt: timestamp };
        case 'plugin:window|set_title': return null;
        default:
          if (cmd.startsWith('list_')) return [];
          if (cmd.startsWith('subscribe_')) return null;
          unknown.push({ cmd, args });
          return { ok: true };
      }
    };
    window.__TAURI_INTERNALS__ = {
      metadata: { currentWindow: { label: 'main' }, currentWebview: { windowLabel: 'main', label: 'main' } },
      invoke,
      transformCallback: (callback) => { callbackId += 1; if (typeof callback === 'function') callbacks.set(callbackId, callback); return callbackId; },
      unregisterCallback: (id) => callbacks.delete(id),
    };
    window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => undefined };
  }, { project, worktrees, sessions, files, commits, replay, timestamp });

  await page.route('**/favicon.ico', (route) => route.fulfill({ status: 204, body: '' }));
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto('http://127.0.0.1:51872/workbench?projectId=demo-project', { waitUntil: 'networkidle' });
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await page.evaluate(async () => { await document.fonts.ready; });
  await page.waitForTimeout(1800);
  await page.screenshot({
    path: '/Users/hans/web_project/cc-partner/docs/media/cc-partner/cc-partner-workbench.png',
    fullPage: false,
    animations: 'disabled',
    caret: 'hide',
    scale: 'css',
  });
  return {
    title: await page.title(),
    url: page.url(),
    text: (await page.locator('body').innerText()).slice(0, 2500),
    unknown: await page.evaluate(() => window.__ccPartnerCaptureUnknown ?? []),
  };
}
