import 'package:cc_partner_mobile/address_book/book.dart';
import 'package:cc_partner_mobile/core/lan_http.dart';
import 'package:cc_partner_mobile/projects/client.dart';
import 'package:cc_partner_mobile/transfer/api.dart';
import 'package:cc_partner_mobile/ui/projects_page.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

class _FakeProjectsClient extends ProjectsClient {
  _FakeProjectsClient({this.failLocalDirOnce = false})
      : super(LanHttpClient(), 'http://127.0.0.1:1');

  final bool failLocalDirOnce;
  final List<String> removedIds = [];
  String? openedPath;
  String? openedRemoteDeviceId;
  String? openedRemotePath;
  String? createdParent;
  String? createdName;
  int _localDirCalls = 0;

  /// 非 null 时返回 lan-fleet 快照；null 时抛错（数据不可得 → 摘要行隐藏）。
  Map<String, dynamic>? fleetSnapshotPayload;
  int fleetCalls = 0;

  /// pathInfo 预检控制：kind/readable 可配，failPathInfo 置 true 时抛错。
  bool failPathInfo = false;
  String infoKind = 'dir';
  bool infoReadable = true;
  int pathInfoCalls = 0;
  String? lastInfoPath;
  String? lastInfoDeviceId;

  @override
  Future<ProjectPathInfo> localPathInfo(String path) async {
    pathInfoCalls += 1;
    lastInfoPath = path;
    lastInfoDeviceId = null;
    if (failPathInfo) {
      throw Exception('info 失败');
    }
    return ProjectPathInfo(path: path, kind: infoKind, readable: infoReadable);
  }

  @override
  Future<ProjectPathInfo> remotePathInfo({
    required String deviceId,
    required String path,
  }) async {
    pathInfoCalls += 1;
    lastInfoPath = path;
    lastInfoDeviceId = deviceId;
    if (failPathInfo) {
      throw Exception('info 失败');
    }
    return ProjectPathInfo(path: path, kind: infoKind, readable: infoReadable);
  }

  @override
  Future<List<ProjectSummary>> listRecent() async => [
        const ProjectSummary(id: 'p1', name: 'demo', kind: 'local', path: '/Users/demo'),
      ];

  @override
  Future<void> remove(String projectId) async {
    removedIds.add(projectId);
  }

  @override
  Future<Map<String, dynamic>> fleetSnapshot() async {
    fleetCalls += 1;
    final payload = fleetSnapshotPayload;
    if (payload == null) {
      throw Exception('fleet down');
    }
    return payload;
  }

  @override
  Future<List<Map<String, dynamic>>> listLocalRoots() async => [
        {'path': '/Users/demo', 'label': 'Home'},
      ];

  @override
  Future<List<Map<String, dynamic>>> listLocalDir(String path) async {
    _localDirCalls += 1;
    if (failLocalDirOnce && _localDirCalls == 1) {
      throw Exception('list-dir 失败');
    }
    if (path == '/Users/demo') {
      return [
        {'name': 'proj-a', 'path': '/Users/demo/proj-a', 'kind': 'dir'},
        {'name': 'notes.txt', 'path': '/Users/demo/notes.txt', 'kind': 'file'},
      ];
    }
    return const [];
  }

  @override
  Future<ProjectSummary> open({required String path, String? deviceId}) async {
    openedPath = path;
    return ProjectSummary(id: 'p-open', name: 'opened', kind: 'local', path: path);
  }

  @override
  Future<Map<String, dynamic>> createDir({
    required String parentPath,
    required String name,
  }) async {
    createdParent = parentPath;
    createdName = name;
    return {'path': '$parentPath/$name', 'kind': 'directory'};
  }

  @override
  Future<List<Map<String, dynamic>>> listRemoteRoots(String deviceId) async => [
        {'path': '/srv', 'label': 'srv'},
      ];

  @override
  Future<List<Map<String, dynamic>>> listRemoteDir({
    required String deviceId,
    required String path,
  }) async {
    if (path == '/srv') {
      return [
        {'name': 'proj-b', 'path': '/srv/proj-b', 'kind': 'dir'},
      ];
    }
    return const [];
  }

  @override
  Future<ProjectSummary> openRemote({
    required String deviceId,
    required String path,
  }) async {
    openedRemoteDeviceId = deviceId;
    openedRemotePath = path;
    return ProjectSummary(id: 'p-lan', name: 'lan', kind: 'remote', path: path);
  }
}

/// 设备列表可配置的假 TransferApi：默认直连在线 peer + 本机（选择器会过滤本机）。
class _FakeTransferApi extends TransferApi {
  _FakeTransferApi([List<Map<String, dynamic>>? devices])
      : devices = devices ??
            [
              {'id': 'peer-1', 'name': 'Laptop', 'isSelf': false, 'status': 'online'},
              {'id': 'host-1', 'name': 'Desktop', 'isSelf': true, 'status': 'online'},
            ],
        super(LanHttpClient(), 'http://127.0.0.1:1');

  final List<Map<String, dynamic>> devices;

  @override
  Future<List<Map<String, dynamic>>> listDevices() async => devices;
}

/// listRecent 可控失败的假客户端：错误态/重试/空态/自定义行用。
class _FlakyListProjectsClient extends ProjectsClient {
  _FlakyListProjectsClient({this.empty = false, this.items})
      : super(LanHttpClient(), 'http://127.0.0.1:1');

  /// 非 null 时 listRecent 抛错；置回 null 后返回项目列表。
  Object? listError;

  /// 置为 true 时返回空列表（空态用）。
  final bool empty;

  /// 非 null 时优先返回该列表（kind 徽章/设备名用）。
  final List<ProjectSummary>? items;
  int listCalls = 0;

  @override
  Future<List<ProjectSummary>> listRecent() async {
    listCalls += 1;
    final error = listError;
    if (error != null) {
      throw error;
    }
    if (empty) {
      return const [];
    }
    final custom = items;
    if (custom != null) {
      return custom;
    }
    return [
      const ProjectSummary(id: 'p1', name: 'demo', kind: 'local', path: '/Users/demo'),
    ];
  }
}

AddressBook _book() => AddressBook(store: MemoryAddressBookStore());

Text _pathText(WidgetTester tester) =>
    tester.widget<Text>(find.byKey(const Key('picker-path')));

Future<void> _pumpPage(
  WidgetTester tester, {
  required _FakeProjectsClient client,
  ValueChanged<ProjectSummary>? onOpen,
  void Function(String projectId)? onProjectRemoved,
  Future<bool> Function(ProjectSummary project)? confirmRemove,
  String? activeProjectId,
  TransferApi? transferApi,
}) async {
  await tester.pumpWidget(
    MaterialApp(
      home: Scaffold(
        body: ProjectsPage(
          book: _book(),
          http: LanHttpClient(),
          onOpen: onOpen ?? (_) {},
          onProjectRemoved: onProjectRemoved,
          confirmRemove: confirmRemove,
          activeProjectId: activeProjectId,
          client: client,
          transferApi: transferApi ?? _FakeTransferApi(),
        ),
      ),
    ),
  );
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('本机目录浏览：进入子目录后打开为项目', (tester) async {
    final client = _FakeProjectsClient();
    ProjectSummary? opened;
    await _pumpPage(tester, client: client, onOpen: (project) => opened = project);

    await tester.tap(find.byKey(const Key('project-add-local')));
    await tester.pumpAndSettle();
    expect(_pathText(tester).data, '/Users/demo');
    expect(find.text('这个目录是空的'), findsNothing);
    // 文件展示但不可进入，目录可进入。
    final fileTile = tester.widget<ListTile>(
      find.byKey(const Key('picker-entry-notes.txt')),
    );
    expect(fileTile.onTap, isNull);
    await tester.tap(find.byKey(const Key('picker-entry-proj-a')));
    await tester.pumpAndSettle();
    expect(_pathText(tester).data, '/Users/demo/proj-a');
    expect(find.text('这个目录是空的'), findsOneWidget);

    await tester.tap(find.byKey(const Key('picker-open')));
    await tester.pumpAndSettle();
    expect(client.openedPath, '/Users/demo/proj-a');
    expect(opened?.id, 'p-open');
    // 选择器已关闭，回到项目列表。
    expect(find.byKey(const Key('picker-open')), findsNothing);
    expect(find.byKey(const Key('project-add-local')), findsOneWidget);
  });

  testWidgets('本机新建文件夹：非法名禁用，合法名创建后进入新目录', (tester) async {
    final client = _FakeProjectsClient();
    await _pumpPage(tester, client: client);

    await tester.tap(find.byKey(const Key('project-add-local')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('picker-create')));
    await tester.pumpAndSettle();

    FilledButton confirmButton() => tester.widget<FilledButton>(
          find.byKey(const Key('picker-create-confirm')),
        );
    // 空名禁用。
    expect(confirmButton().onPressed, isNull);
    // 含斜杠禁用。
    await tester.enterText(find.byKey(const Key('picker-create-name')), 'a/b');
    await tester.pump();
    expect(confirmButton().onPressed, isNull);
    // 合法名可提交。
    await tester.enterText(find.byKey(const Key('picker-create-name')), 'ok-dir');
    await tester.pump();
    expect(confirmButton().onPressed, isNotNull);
    await tester.tap(find.byKey(const Key('picker-create-confirm')));
    await tester.pumpAndSettle();

    expect(client.createdParent, '/Users/demo');
    expect(client.createdName, 'ok-dir');
    expect(_pathText(tester).data, '/Users/demo/ok-dir');
    expect(find.byKey(const Key('picker-create-name')), findsNothing);
  });

  testWidgets('删除项目：先弹确认框，取消不删，确认才调用 remove', (tester) async {
    final client = _FakeProjectsClient();
    await _pumpPage(tester, client: client);

    await tester.tap(find.byKey(const Key('project-remove-p1')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('remove-confirm')), findsOneWidget);
    expect(find.text('只会从这台电脑的最近列表移除，不会删除文件。'), findsOneWidget);
    expect(client.removedIds, isEmpty);

    await tester.tap(find.byKey(const Key('remove-confirm-cancel')));
    await tester.pumpAndSettle();
    expect(client.removedIds, isEmpty);

    await tester.tap(find.byKey(const Key('project-remove-p1')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('remove-confirm-accept')));
    await tester.pumpAndSettle();
    expect(client.removedIds, ['p1']);
  });

  testWidgets('删除成功后以已删除项目 id 回调 onProjectRemoved', (tester) async {
    final client = _FakeProjectsClient();
    final removed = <String>[];
    await _pumpPage(
      tester,
      client: client,
      onProjectRemoved: removed.add,
    );

    // 取消路径：不回调。
    await tester.tap(find.byKey(const Key('project-remove-p1')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('remove-confirm-cancel')));
    await tester.pumpAndSettle();
    expect(removed, isEmpty);

    // 确认删除成功：以项目 id 回调（接缝契约：壳层清理激活项目上下文）。
    await tester.tap(find.byKey(const Key('project-remove-p1')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('remove-confirm-accept')));
    await tester.pumpAndSettle();
    expect(removed, ['p1']);
  });

  testWidgets('fleet 数据可得时展示 agent fleet 摘要行', (tester) async {
    final client = _FakeProjectsClient()
      ..fleetSnapshotPayload = {
        'generatedAt': '2026-09-20T00:00:00Z',
        'devices': [
          {
            'deviceId': 'd1',
            'deviceName': 'Laptop',
            'reachability': 'live',
            'projects': [
              {
                'projectId': 'p1',
                'agentCounts': {'needsInput': 1, 'failed': 1, 'working': 2},
              },
              {
                'projectId': 'p2',
                'agentCounts': {'failed': 1},
              },
            ],
          },
          {
            'deviceId': 'd2',
            'deviceName': 'Desktop',
            'reachability': 'offline',
            'projects': <dynamic>[],
          },
        ],
      };
    await _pumpPage(tester, client: client);

    expect(find.byKey(const Key('projects-fleet-summary')), findsOneWidget);
    // 口径对齐 web：needsInput+failed 合计 3；离线 1 台；working 不计异常。
    expect(find.text('局域网 Agent Fleet · 3 需处理 · 设备离线 (1)'), findsOneWidget);
  });

  testWidgets('fleet 无异常时摘要只显示标题', (tester) async {
    final client = _FakeProjectsClient()
      ..fleetSnapshotPayload = {
        'devices': [
          {
            'deviceId': 'd1',
            'reachability': 'live',
            'projects': [
              {
                'projectId': 'p1',
                'agentCounts': {'needsInput': 0, 'failed': 0},
              },
            ],
          },
        ],
      };
    await _pumpPage(tester, client: client);

    expect(find.text('局域网 Agent Fleet'), findsOneWidget);
  });

  testWidgets('fleet 数据不可得时整行隐藏不占位', (tester) async {
    final client = _FakeProjectsClient();
    await _pumpPage(tester, client: client);

    expect(client.fleetCalls, 1);
    expect(find.byKey(const Key('projects-fleet-summary')), findsNothing);
  });

  test('fleet 摘要解析：缺 devices 返回 null，负数计数按 0 饱和', () {
    expect(LanFleetOverview.fromSnapshot(<String, dynamic>{}), isNull);
    final overview = LanFleetOverview.fromSnapshot({
      'devices': [
        {
          'reachability': 'offline',
          'projects': [
            {
              'agentCounts': {'needsInput': -2, 'failed': 3},
            },
          ],
        },
        {'reachability': 'live', 'projects': <dynamic>[]},
      ],
    });
    expect(overview, isNotNull);
    expect(overview!.offlineDevices, 1);
    expect(overview.exceptionAgents, 3);
    expect(overview.label, '局域网 Agent Fleet · 3 需处理 · 设备离线 (1)');
  });

  testWidgets('目录加载失败：展示错误并支持重试', (tester) async {
    final client = _FakeProjectsClient(failLocalDirOnce: true);
    await _pumpPage(tester, client: client);

    await tester.tap(find.byKey(const Key('project-add-local')));
    await tester.pumpAndSettle();
    expect(find.textContaining('list-dir 失败'), findsOneWidget);
    expect(find.byKey(const Key('picker-entry-proj-a')), findsNothing);

    await tester.tap(find.byKey(const Key('picker-retry')));
    await tester.pumpAndSettle();
    expect(find.textContaining('list-dir 失败'), findsNothing);
    expect(find.byKey(const Key('picker-entry-proj-a')), findsOneWidget);
  });

  testWidgets('局域网：设备列表过滤离线/自身并去重影子，选中后浏览经远端打开',
      (tester) async {
    final client = _FakeProjectsClient();
    ProjectSummary? opened;
    // 设备口径：直连在线 peer + 同 id 影子重复条目（去重）+ 离线 peer（过滤）+
    // 本机（过滤）+ 仅影子可见的在线 peer（保留，带「经 NAS 中转」）。
    final transfer = _FakeTransferApi([
      {'id': 'peer-1', 'name': 'Laptop', 'isSelf': false, 'status': 'online', 'address': '192.168.1.2'},
      {
        'id': 'peer-1',
        'name': 'Laptop',
        'isSelf': false,
        'status': 'online',
        'viaDeviceId': 'nas',
        'viaDeviceName': 'NAS',
      },
      {'id': 'peer-2', 'name': 'Down', 'isSelf': false, 'status': 'offline'},
      {'id': 'host-1', 'name': 'Desktop', 'isSelf': true, 'status': 'online'},
      {
        'id': 'peer-3',
        'name': 'Home Server',
        'isSelf': false,
        'status': 'online',
        'viaDeviceId': 'nas',
        'viaDeviceName': 'NAS',
      },
    ]);
    await _pumpPage(tester, client: client, onOpen: (project) => opened = project, transferApi: transfer);

    await tester.tap(find.byKey(const Key('project-add-lan')));
    await tester.pumpAndSettle();
    // 必须先选设备才能浏览：还没有「打开此目录」。
    expect(find.byKey(const Key('picker-open')), findsNothing);
    // 本机与离线设备不可选；同设备直连+影子只出现一次。
    expect(find.text('Desktop · 主机'), findsNothing);
    expect(find.byKey(const Key('picker-device-peer-2')), findsNothing);
    expect(find.byKey(const Key('picker-device-peer-1')), findsOneWidget);
    // 仅影子可见的设备带「经 NAS 中转」徽标；直连设备没有。
    expect(find.byKey(const Key('picker-device-via-peer-3')), findsOneWidget);
    expect(find.text('经 NAS 中转'), findsOneWidget);
    expect(find.byKey(const Key('picker-device-via-peer-1')), findsNothing);
    // 直连排在前、影子在后。
    final directY = tester.getTopLeft(find.byKey(const Key('picker-device-peer-1'))).dy;
    final shadowY = tester.getTopLeft(find.byKey(const Key('picker-device-peer-3'))).dy;
    expect(directY, lessThan(shadowY));

    await tester.tap(find.byKey(const Key('picker-device-peer-1')));
    await tester.pumpAndSettle();
    expect(_pathText(tester).data, '/srv');
    await tester.tap(find.byKey(const Key('picker-entry-proj-b')));
    await tester.pumpAndSettle();
    expect(_pathText(tester).data, '/srv/proj-b');
    // 打开前对远端路径做了 info 预检。
    expect(client.lastInfoDeviceId, 'peer-1');
    expect(client.lastInfoPath, '/srv/proj-b');

    await tester.tap(find.byKey(const Key('picker-open')));
    await tester.pumpAndSettle();
    expect(client.openedRemoteDeviceId, 'peer-1');
    expect(client.openedRemotePath, '/srv/proj-b');
    expect(opened?.id, 'p-lan');
    expect(find.byKey(const Key('picker-open')), findsNothing);
  });

  testWidgets('局域网新建文件夹：对端缺 mkdir 能力时隐藏按钮', (tester) async {
    // 无 capabilities 的对端（旧 peer）。
    final client = _FakeProjectsClient();
    await _pumpPage(
      tester,
      client: client,
      transferApi: _FakeTransferApi([
        {'id': 'peer-1', 'name': 'Laptop', 'isSelf': false, 'status': 'online'},
      ]),
    );
    await tester.tap(find.byKey(const Key('project-add-lan')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('picker-device-peer-1')));
    await tester.pumpAndSettle();
    expect(_pathText(tester).data, '/srv');
    // 缺 workbench.fs.create-dir.v1：「新建文件夹」整体隐藏、不回落。
    expect(find.byKey(const Key('picker-create')), findsNothing);
    // 「打开此目录」不受 mkdir 能力影响，预检通过后可用。
    expect(
      tester.widget<FilledButton>(find.byKey(const Key('picker-open'))).onPressed,
      isNotNull,
    );
  });

  testWidgets('局域网新建文件夹：capabilities 含 create-dir.v1 时按钮可用', (tester) async {
    final capable = _FakeProjectsClient();
    await _pumpPage(
      tester,
      client: capable,
      transferApi: _FakeTransferApi([
        {
          'id': 'peer-1',
          'name': 'Laptop',
          'isSelf': false,
          'status': 'online',
          'capabilities': ['transfer.resume.v1', 'workbench.fs.create-dir.v1'],
        },
      ]),
    );
    await tester.tap(find.byKey(const Key('project-add-lan')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('picker-device-peer-1')));
    await tester.pumpAndSettle();
    final createButton = tester.widget<OutlinedButton>(
      find.byKey(const Key('picker-create')),
    );
    expect(createButton.onPressed, isNotNull);
  });

  testWidgets('打开此目录预检：info 失败时禁用，预检通过才允许打开', (tester) async {
    final client = _FakeProjectsClient()..failPathInfo = true;
    ProjectSummary? opened;
    await _pumpPage(tester, client: client, onOpen: (project) => opened = project);

    await tester.tap(find.byKey(const Key('project-add-local')));
    await tester.pumpAndSettle();
    // 进入子目录后预检失败：「打开此目录」禁用，点按不会触发 open。
    final openButton = find.byKey(const Key('picker-open'));
    expect(
      tester.widget<FilledButton>(openButton).onPressed,
      isNull,
    );
    await tester.tap(openButton);
    await tester.pumpAndSettle();
    expect(client.openedPath, isNull);
    expect(opened, isNull);

    // 预检恢复（重新进入子目录触发新预检）后可以打开。
    client.failPathInfo = false;
    await tester.tap(find.byKey(const Key('picker-entry-proj-a')));
    await tester.pumpAndSettle();
    expect(_pathText(tester).data, '/Users/demo/proj-a');
    expect(
      tester.widget<FilledButton>(openButton).onPressed,
      isNotNull,
    );
    await tester.tap(openButton);
    await tester.pumpAndSettle();
    expect(client.openedPath, '/Users/demo/proj-a');
    expect(opened?.id, 'p-open');
  });

  testWidgets('打开此目录预检：info 为文件时禁用', (tester) async {
    final client = _FakeProjectsClient()..infoKind = 'file';
    await _pumpPage(tester, client: client);
    await tester.tap(find.byKey(const Key('project-add-local')));
    await tester.pumpAndSettle();
    expect(
      tester.widget<FilledButton>(find.byKey(const Key('picker-open'))).onPressed,
      isNull,
    );
  });

  testWidgets('打开此目录预检：info 不可读时禁用', (tester) async {
    final client = _FakeProjectsClient()..infoReadable = false;
    await _pumpPage(tester, client: client);
    await tester.tap(find.byKey(const Key('project-add-local')));
    await tester.pumpAndSettle();
    expect(
      tester.widget<FilledButton>(find.byKey(const Key('picker-open'))).onPressed,
      isNull,
    );
  });

  testWidgets('列表加载失败：错误卡 + 重试按钮 + 重试成功后回到列表', (tester) async {
    final client = _FlakyListProjectsClient();
    client.listError = Exception('list 失败');
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: ProjectsPage(
            book: _book(),
            http: LanHttpClient(),
            onOpen: (_) {},
            client: client,
            transferApi: _FakeTransferApi(),
          ),
        ),
      ),
    );
    await tester.pumpAndSettle();

    // 错误卡语义：明确「加载失败」+ 原因 + 重试入口。
    expect(find.byKey(const Key('projects-error-card')), findsOneWidget);
    expect(find.text('加载失败'), findsOneWidget);
    expect(find.textContaining('list 失败'), findsOneWidget);

    // 重试成功后恢复列表。
    client.listError = null;
    await tester.tap(find.byKey(const Key('projects-error-retry')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('projects-error-card')), findsNothing);
    expect(find.text('demo'), findsOneWidget);
  });

  testWidgets('空项目列表显示 web 同款空态文案', (tester) async {
    final client = _FlakyListProjectsClient(empty: true);
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: ProjectsPage(
            book: _book(),
            http: LanHttpClient(),
            onOpen: (_) {},
            client: client,
            transferApi: _FakeTransferApi(),
          ),
        ),
      ),
    );
    await tester.pumpAndSettle();
    expect(find.text('还没有项目文件夹'), findsOneWidget);
  });

  testWidgets('删除顺序：先弹确认框，取消不触发 dirty 预检也不调后端', (tester) async {
    final client = _FakeProjectsClient();
    final hookCalls = <ProjectSummary>[];
    await _pumpPage(
      tester,
      client: client,
      confirmRemove: (project) async {
        hookCalls.add(project);
        return true;
      },
    );

    // 确认框先出现，此时 dirty 预检尚未触发（对齐 web：点「移除」才预检）。
    await tester.tap(find.byKey(const Key('project-remove-p1')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('remove-confirm')), findsOneWidget);
    expect(hookCalls, isEmpty);
    expect(client.removedIds, isEmpty);

    // 取消：不触发预检、不删除、不回调。
    await tester.tap(find.byKey(const Key('remove-confirm-cancel')));
    await tester.pumpAndSettle();
    expect(hookCalls, isEmpty);
    expect(client.removedIds, isEmpty);
  });

  testWidgets('确认后先做 dirty 预检：返回 false 静默中止，不调 remove', (tester) async {
    final client = _FakeProjectsClient();
    final removed = <String>[];
    final hookCalls = <ProjectSummary>[];
    await _pumpPage(
      tester,
      client: client,
      onProjectRemoved: removed.add,
      confirmRemove: (project) async {
        hookCalls.add(project);
        return false;
      },
    );

    await tester.tap(find.byKey(const Key('project-remove-p1')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('remove-confirm-accept')));
    await tester.pumpAndSettle();

    // 预检以完整项目为参且在确认后被调用；返回 false 不调后端、不回调壳层。
    expect(hookCalls.single.id, 'p1');
    expect(client.removedIds, isEmpty);
    expect(removed, isEmpty);
  });

  testWidgets('confirmRemove 返回 true：预检通过后删除并回调', (tester) async {
    final client = _FakeProjectsClient();
    final removed = <String>[];
    await _pumpPage(
      tester,
      client: client,
      onProjectRemoved: removed.add,
      confirmRemove: (project) async => true,
    );

    await tester.tap(find.byKey(const Key('project-remove-p1')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('remove-confirm-accept')));
    await tester.pumpAndSettle();

    expect(client.removedIds, ['p1']);
    expect(removed, ['p1']);
  });

  testWidgets('项目行显示 kind 徽章与 remote 设备名', (tester) async {
    final client = _FlakyListProjectsClient(items: [
      const ProjectSummary(
        id: 'p-remote',
        name: 'lan-proj',
        kind: 'remote',
        path: '/srv/lan',
        deviceName: 'Laptop',
      ),
      const ProjectSummary(
        id: 'p-local',
        name: 'local-proj',
        kind: 'local',
        path: '/Users/demo',
      ),
      // 有 path：副标题显示 path，避免与 kind 徽章文本重复。
      const ProjectSummary(
          id: 'p-other', name: 'other-proj', kind: 'bridge', path: '/mnt/bridge'),
      const ProjectSummary(
        id: 'p-remote-nd',
        name: 'lan-nd',
        kind: 'remote',
        path: '/srv/nd',
      ),
    ]);
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: ProjectsPage(
            book: _book(),
            http: LanHttpClient(),
            onOpen: (_) {},
            client: client,
            transferApi: _FakeTransferApi(),
          ),
        ),
      ),
    );
    await tester.pumpAndSettle();

    // remote 行：徽章「远端」+ 设备名（p-remote 与 p-remote-nd 两行均为 remote）。
    expect(find.byKey(const Key('project-kind-badge-p-remote')), findsOneWidget);
    expect(find.text('远端'), findsNWidgets(2));
    expect(
      find.byKey(const Key('project-device-name-p-remote')),
      findsOneWidget,
    );
    expect(find.text('Laptop'), findsOneWidget);

    // local 行：徽章「本机项目」，无设备名。
    expect(find.byKey(const Key('project-kind-badge-p-local')), findsOneWidget);
    expect(find.text('本机项目'), findsOneWidget);
    expect(
      find.byKey(const Key('project-device-name-p-local')),
      findsNothing,
    );

    // 其它 kind 原样文本；remote 缺 deviceName 时不显示设备名。
    expect(find.byKey(const Key('project-kind-badge-p-other')), findsOneWidget);
    expect(find.text('bridge'), findsOneWidget);
    expect(
      find.byKey(const Key('project-device-name-p-remote-nd')),
      findsNothing,
    );
  });

  testWidgets('不支持类型（kind 非 local/remote 或缺失）的项目行禁点并显示说明',
      (tester) async {
    ProjectSummary? opened;
    final client = _FlakyListProjectsClient(items: [
      const ProjectSummary(
          id: 'p-tpl', name: 'tpl-proj', kind: 'template', path: '/mnt/tpl'),
      const ProjectSummary(id: 'p-nokind', name: 'nokind-proj', path: '/mnt/nokind'),
      const ProjectSummary(id: 'p-ok', name: 'ok-proj', kind: 'local', path: '/Users/demo'),
    ]);
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: ProjectsPage(
            book: _book(),
            http: LanHttpClient(),
            onOpen: (project) => opened = project,
            client: client,
            transferApi: _FakeTransferApi(),
          ),
        ),
      ),
    );
    await tester.pumpAndSettle();

    // 非 local/remote（含缺 kind）行：onTap 为 null + enabled=false 置灰，
    // 「不支持的项目类型」说明上屏（对齐 web canSelectMobileProject fail-closed）。
    final tplTile = tester.widget<ListTile>(
      find.byKey(const Key('project-row-p-tpl')),
    );
    expect(tplTile.onTap, isNull);
    expect(tplTile.enabled, isFalse);
    final nokindTile = tester.widget<ListTile>(
      find.byKey(const Key('project-row-p-nokind')),
    );
    expect(nokindTile.onTap, isNull);
    expect(nokindTile.enabled, isFalse);
    expect(find.text('不支持的项目类型'), findsNWidgets(2));

    // 点不可选行不触发打开。
    await tester.tap(find.text('tpl-proj'));
    await tester.pumpAndSettle();
    expect(opened, isNull);

    // local 行不受影响：可点开且不渲染说明。
    final okTile = tester.widget<ListTile>(
      find.byKey(const Key('project-row-p-ok')),
    );
    expect(okTile.onTap, isNotNull);
    expect(okTile.enabled, isTrue);
    expect(find.byKey(const Key('project-unsupported-p-ok')), findsNothing);
    await tester.tap(find.text('ok-proj'));
    await tester.pumpAndSettle();
    expect(opened?.id, 'p-ok');
  });

  testWidgets('下拉刷新同步重拉 fleet 摘要', (tester) async {
    final client = _FakeProjectsClient();
    await _pumpPage(tester, client: client);
    // 首载失败：摘要行隐藏。
    expect(client.fleetCalls, 1);
    expect(find.byKey(const Key('projects-fleet-summary')), findsNothing);

    // 下拉刷新：fleet 与项目列表一起重拉，摘要行出现。
    client.fleetSnapshotPayload = {
      'devices': [
        {
          'deviceId': 'd1',
          'reachability': 'offline',
          'projects': [
            {
              'agentCounts': {'needsInput': 2},
            },
          ],
        },
      ],
    };
    await tester.fling(find.byType(ListView), const Offset(0, 400), 1200);
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 600));
    await tester.pumpAndSettle();
    expect(client.fleetCalls, 2);
    expect(find.byKey(const Key('projects-fleet-summary')), findsOneWidget);
    expect(find.text('局域网 Agent Fleet · 2 需处理 · 设备离线 (1)'), findsOneWidget);
  });

  testWidgets('activeProjectId 命中行高亮并带 selected 语义', (tester) async {
    final client = _FlakyListProjectsClient(items: [
      const ProjectSummary(id: 'p1', name: 'demo', kind: 'local', path: '/Users/demo'),
      const ProjectSummary(id: 'p2', name: 'other', kind: 'local', path: '/Users/other'),
    ]);
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: ProjectsPage(
            book: _book(),
            http: LanHttpClient(),
            onOpen: (_) {},
            activeProjectId: 'p1',
            client: client,
            transferApi: _FakeTransferApi(),
          ),
        ),
      ),
    );
    await tester.pumpAndSettle();

    // 命中行：Semantics(selected: true) + 视觉高亮（tileColor）。
    final activeSemantics = tester.widget<Semantics>(
      find.byKey(const Key('project-row-active-p1')),
    );
    expect(activeSemantics.properties.selected, isTrue);
    final activeTile = tester.widget<ListTile>(
      find.byKey(const Key('project-row-p1')),
    );
    expect(activeTile.tileColor, isNotNull);
    // 未命中行：无 selected 语义、无高亮。
    expect(find.byKey(const Key('project-row-active-p2')), findsNothing);
    final idleTile = tester.widget<ListTile>(
      find.byKey(const Key('project-row-p2')),
    );
    expect(idleTile.tileColor, isNull);

    // activeProjectId 为 null（默认）：无高亮行（现有调用点不受影响）。
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: ProjectsPage(
            book: _book(),
            http: LanHttpClient(),
            onOpen: (_) {},
            client: client,
            transferApi: _FakeTransferApi(),
          ),
        ),
      ),
    );
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('project-row-active-p1')), findsNothing);
  });
}
