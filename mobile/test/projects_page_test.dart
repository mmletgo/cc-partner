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

  @override
  Future<List<ProjectSummary>> listRecent() async => [
        const ProjectSummary(id: 'p1', name: 'demo', kind: 'local', path: '/Users/demo'),
      ];

  @override
  Future<void> remove(String projectId) async {
    removedIds.add(projectId);
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

/// listRecent 可控失败的假客户端：错误态/重试/空态用。
class _FlakyListProjectsClient extends ProjectsClient {
  _FlakyListProjectsClient({this.empty = false}) : super(LanHttpClient(), 'http://127.0.0.1:1');

  /// 非 null 时 listRecent 抛错；置回 null 后返回项目列表。
  Object? listError;

  /// 置为 true 时返回空列表（空态用）。
  final bool empty;
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
}) async {
  await tester.pumpWidget(
    MaterialApp(
      home: Scaffold(
        body: ProjectsPage(
          book: _book(),
          http: LanHttpClient(),
          onOpen: onOpen ?? (_) {},
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
}
