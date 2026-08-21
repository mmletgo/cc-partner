async (page) => {
  const timestamp = '2026-08-21T12:00:00.000Z';
  const project = {
    id: 'mobile-demo-project', name: 'cc-partner-demo', kind: 'local', deviceId: 'demo-mac',
    deviceName: 'Demo Mac', path: '/workspace/cc-partner-demo', lastOpenedAt: timestamp,
    createdAt: timestamp, updatedAt: timestamp,
  };
  const worktree = {
    id: 'mobile-demo-main', projectId: project.id, name: 'main', branch: 'main', baseBranch: null,
    path: project.path, isMain: true,
    status: { branch: 'main', changed: 2, ahead: 1, behind: 0, conflicts: 0, clean: false, canPush: true },
    createdAt: timestamp, updatedAt: timestamp,
  };
  const session = {
    id: 'mobile-session', projectId: project.id, worktreeId: worktree.id, name: 'Coding Agent · implementation',
    command: '/bin/zsh', cwd: project.path, status: 'running', cols: 80, rows: 24,
    startedAt: timestamp, exitedAt: null, exitCode: null, supportsPanes: true, paneCount: 1,
  };
  const note = { name: 'release-notes.md', path: 'docs/release-notes.md', kind: 'file', size: 368, modifiedAt: timestamp, children: null };
  const routes = new Map([
    ['GET /api/health', { ok: true, status: 'ok', protocol_version: 1, capabilities: ['attention.v1', 'orchestrator.runtime-snapshot.v1', 'errors.envelope.v1'], http_port: 62116 }],
    ['GET /api/mobile/attention', { generatedAt: timestamp, counts: { total: 1, decision: 1, blocked: 0, environment: 0, unreadTotal: 1, unreadDecision: 1, unreadBlocked: 0, unreadEnvironment: 0 }, myDeviceId: 'demo-mac', items: [{ id: 'orchestrator:review:demo', category: 'decision', sourceKind: 'orchestratorHumanReview', title: '等待交付复核', summary: '验证通过，等待确认合并', updatedAt: timestamp, freshness: 'live', cachedAt: null, project: { id: project.id, name: project.name, kind: 'local' }, device: null, target: { kind: 'orchestratorTask', projectId: project.id, taskId: 'task-demo' } }] }],
    ['GET /api/mobile/workbench/projects/list', [project]],
    ['POST /api/mobile/workbench/worktrees/list', [worktree]],
    ['POST /api/mobile/workbench/sessions/list', [session]],
    ['POST /api/mobile/workbench/sessions/replay', { sessionId: session.id, buffer: 'cc-partner-demo  main\n✓ build passed\n✓ mobile smoke passed\n等待交付复核\n', truncated: false, lastSeq: 8 }],
    ['POST /api/mobile/workbench/sessions/focus', { ok: true, sessionId: session.id }],
    ['POST /api/mobile/workbench/sessions/resize', { ok: true, sessionId: session.id }],
    ['POST /api/mobile/workbench/sessions/zoom-pane', { ok: true, sessionId: session.id }],
    ['POST /api/mobile/workbench/files/list-dir', [
      { name: 'web', path: 'web', kind: 'directory', size: null, modifiedAt: timestamp, children: null },
      { name: 'src-tauri', path: 'src-tauri', kind: 'directory', size: null, modifiedAt: timestamp, children: null },
      { name: 'docs', path: 'docs', kind: 'directory', size: null, modifiedAt: timestamp, children: null },
      note,
      { name: 'README.md', path: 'README.md', kind: 'file', size: 8421, modifiedAt: timestamp, children: null },
    ]],
    ['POST /api/mobile/workbench/files/info', note],
    ['POST /api/mobile/workbench/files/open', { metadata: note, detectedType: 'markdown', capabilities: { canPreview: true, canEdit: true, canFormat: false, mustValidateBeforeSave: false, defaultMode: 'editor', availableModes: ['editor', 'source'] }, text: { content: '# 交付检查\n\n- [x] 前端构建\n- [x] Mobile smoke\n- [x] Git evidence\n- [ ] 人工复核并合并\n\n离开电脑后，仍可在手机上查看状态、编辑文件并继续推进。', baseHash: 'mobile-hash', baseModifiedAt: timestamp }, image: null, csv: null, sqlite: null, truncated: false, notice: null }],
    ['POST /api/mobile/workbench/files/save-text', { metadata: note, baseHash: 'mobile-hash-2', baseModifiedAt: timestamp }],
    ['POST /api/orchestrator/task-views/list', { views: [] }],
    ['POST /api/mobile/orchestrator/runtime-snapshot', { projectId: project.id, projectKind: 'local', remoteStatus: 'local', generatedAt: timestamp, latestTickAt: timestamp, lastDispatchAt: timestamp, lastDispatchedCount: 1, schedulerEnabled: true, workflowSource: 'project', workflowValid: true, workflowError: null, maxConcurrentTasks: 2, slotsUsed: 1, slotsAvailable: 1, latestError: null, runningTasks: [], retryingTasks: [], recentEvents: [] }],
    ['POST /api/orchestrator/tasks/evidence', { evidence: [] }],
    ['GET /api/mobile/devices', [{ id: 'demo-mac', name: 'Demo Mac', address: '127.0.0.1', port: 62116, isSelf: true, online: true, lastSeen: timestamp, protoVersion: 1, capabilities: [] }]],
    ['GET /api/mobile/transfer/tasks', []],
  ]);

  await page.route('**/favicon.ico', (route) => route.fulfill({ status: 204, body: '' }));
  await page.route('**/api/**', async (route) => {
    const request = route.request();
    const path = request.url().replace(/^https?:\/\/[^/]+/, '').split('?')[0];
    if (path !== '/api' && !path.startsWith('/api/')) {
      await route.continue();
      return;
    }
    const key = `${request.method()} ${path}`;
    if (key === 'GET /api/workbench/events') {
      await route.fulfill({ status: 200, contentType: 'application/x-ndjson', body: '' });
      return;
    }
    const body = routes.has(key) ? routes.get(key) : {};
    await route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(body) });
  });
  await page.addInitScript(() => {
    localStorage.setItem('cp-lang', 'zh');
    localStorage.setItem('cp-theme', 'light');
  });
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto('http://127.0.0.1:51872/mobile', { waitUntil: 'networkidle' });
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await page.getByRole('button', { name: /cc-partner-demo/ }).click();
  await page.waitForTimeout(800);
  const openNav = page.getByRole('button', { name: /打开导航/ });
  await openNav.click();
  await page.getByRole('dialog').getByRole('button', { name: /^文件/ }).click();
  await page.waitForTimeout(1200);
  const noteButton = page.getByRole('button', { name: /release-notes\.md/ });
  if (await noteButton.count()) await noteButton.first().click();
  await page.evaluate(async () => { await document.fonts.ready; });
  await page.waitForTimeout(1200);
  await page.screenshot({
    path: '/Users/hans/web_project/cc-partner/docs/media/cc-partner/cc-partner-mobile-workbench.png',
    fullPage: false,
    animations: 'disabled',
    caret: 'hide',
    scale: 'css',
  });
  return { title: await page.title(), url: page.url(), text: (await page.locator('body').innerText()).slice(0, 2200) };
}
