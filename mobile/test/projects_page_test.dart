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

class _FakeTransferApi extends TransferApi {
  _FakeTransferApi() : super(LanHttpClient(), 'http://127.0.0.1:1');

  @override
  Future<List<Map<String, dynamic>>> listDevices() async => [
        {'id': 'peer-1', 'name': 'Laptop', 'isSelf': false},
        {'id': 'host-1', 'name': 'Desktop', 'isSelf': true},
      ];
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
          client: client,
          transferApi: _FakeTransferApi(),
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

  testWidgets('局域网：先选设备（主机置顶）再浏览并经远端打开', (tester) async {
    final client = _FakeProjectsClient();
    ProjectSummary? opened;
    await _pumpPage(tester, client: client, onOpen: (project) => opened = project);

    await tester.tap(find.byKey(const Key('project-add-lan')));
    await tester.pumpAndSettle();
    // 必须先选设备才能浏览：还没有「打开此目录」。
    expect(find.byKey(const Key('picker-open')), findsNothing);
    expect(find.text('Desktop · 主机'), findsOneWidget);
    // 主机置顶。
    final hostY = tester.getTopLeft(find.byKey(const Key('picker-device-host-1'))).dy;
    final peerY = tester.getTopLeft(find.byKey(const Key('picker-device-peer-1'))).dy;
    expect(hostY, lessThan(peerY));

    await tester.tap(find.byKey(const Key('picker-device-host-1')));
    await tester.pumpAndSettle();
    expect(_pathText(tester).data, '/srv');
    await tester.tap(find.byKey(const Key('picker-entry-proj-b')));
    await tester.pumpAndSettle();
    expect(_pathText(tester).data, '/srv/proj-b');

    await tester.tap(find.byKey(const Key('picker-open')));
    await tester.pumpAndSettle();
    expect(client.openedRemoteDeviceId, 'host-1');
    expect(client.openedRemotePath, '/srv/proj-b');
    expect(opened?.id, 'p-lan');
    expect(find.byKey(const Key('picker-open')), findsNothing);
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

  testWidgets('confirmRemove 返回 false：静默中止，不弹确认框也不调用 remove',
      (tester) async {
    final client = _FakeProjectsClient();
    final hooked = <ProjectSummary>[];
    await _pumpPage(
      tester,
      client: client,
      confirmRemove: (project) async {
        hooked.add(project);
        return false;
      },
    );

    await tester.tap(find.byKey(const Key('project-remove-p1')));
    await tester.pumpAndSettle();

    // 钩子以完整项目为参被调用；返回 false 后不弹确认框、不删除。
    expect(hooked.single.id, 'p1');
    expect(find.byKey(const Key('remove-confirm')), findsNothing);
    expect(client.removedIds, isEmpty);
  });

  testWidgets('confirmRemove 返回 true：继续弹确认框，确认后删除', (tester) async {
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
    // 钩子放行后照常弹确认框。
    expect(find.byKey(const Key('remove-confirm')), findsOneWidget);
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
}
