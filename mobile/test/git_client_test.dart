import 'dart:convert';
import 'dart:io';

import 'package:cc_partner_mobile/core/lan_http.dart';
import 'package:cc_partner_mobile/git/client.dart';
import 'package:test/test.dart';

void main() {
  late HttpServer server;
  late String baseUrl;
  String? lastPath;
  Map<String, dynamic> lastBody = const {};
  // /git/commits 的响应体；后端返回裸数组，测试里也覆盖包一层 commits 的形态。
  Object commitsResponse = const [];
  // /worktrees/list 的响应体。真实后端返回裸数组；旧假数据包一层 worktrees。
  Object worktreesResponse = const [
    {
      'id': 'wt-main',
      'name': 'main',
      'branch': 'main',
      'isMain': true,
    },
  ];

  setUp(() async {
    lastPath = null;
    lastBody = const {};
    commitsResponse = const [];
    worktreesResponse = const [
      {
        'id': 'wt-main',
        'name': 'main',
        'branch': 'main',
        'isMain': true,
      },
    ];
    server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    baseUrl = 'http://127.0.0.1:${server.port}';
    server.listen((request) async {
      lastPath = request.uri.path;
      final raw = await utf8.decodeStream(request);
      if (raw.isNotEmpty) {
        lastBody = jsonDecode(raw) as Map<String, dynamic>;
      }
      request.response.headers.contentType = ContentType.json;
      if (request.uri.path.endsWith('/worktrees/list')) {
        request.response.write(jsonEncode(worktreesResponse));
      } else if (request.uri.path.endsWith('/worktrees/create')) {
        request.response.write(
          jsonEncode({
            'id': 'wt-feat',
            'name': lastBody['branchName'],
            'branch': lastBody['branchName'],
            'isMain': false,
          }),
        );
      } else if (request.uri.path.endsWith('/git/commits')) {
        request.response.write(jsonEncode(commitsResponse));
      } else {
        request.response.write(jsonEncode({'ok': true, 'kind': 'succeeded'}));
      }
      await request.response.close();
    });
  });

  tearDown(() async {
    await server.close(force: true);
  });

  test('lists worktrees on the current PC', () async {
    final http = LanHttpClient();
    addTearDown(http.close);
    final git = GitClient(http, baseUrl);
    final body = await git.listWorktrees('p1');
    expect(lastPath, '/api/mobile/workbench/worktrees/list');
    expect(lastBody['includeGitStatus'], isFalse);
    // 后端返回裸数组；客户端归一成 {worktrees: [...]}，供壳层 asObjectList 使用。
    final trees = body['worktrees'] as List;
    expect(trees, hasLength(1));
    expect((trees.first as Map)['id'], 'wt-main');
  });

  test('lists worktrees wrapped in an object', () async {
    worktreesResponse = {
      'ok': true,
      'worktrees': [
        {
          'id': 'wt-main',
          'name': 'main',
          'branch': 'main',
          'isMain': true,
        },
      ],
    };
    final http = LanHttpClient();
    addTearDown(http.close);
    final git = GitClient(http, baseUrl);
    final body = await git.listWorktrees('p1');
    expect(body['ok'], isTrue);
    final trees = body['worktrees'] as List;
    expect((trees.first as Map)['id'], 'wt-main');
  });

  test('lists worktrees with git status when requested', () async {
    final http = LanHttpClient();
    addTearDown(http.close);
    final git = GitClient(http, baseUrl);
    await git.listWorktrees('p1', includeGitStatus: true);
    expect(lastBody['includeGitStatus'], isTrue);
  });

  test('fetches commits for a worktree with refs', () async {
    commitsResponse = [
      {
        'hash': 'a1b2c3d4e5f6',
        'shortHash': 'a1b2c3d',
        'parentHashes': ['fff0001'],
        'authorName': '韩梅梅',
        'authorEmail': 'han@example.com',
        'authoredAt': '2026-09-19T10:30:00Z',
        'summary': 'fix: mobile git history',
        'refs': [
          {
            'name': 'main',
            'fullName': 'refs/heads/main',
            'kind': 'local',
            'remote': null,
            'isHead': true,
          },
          {
            'name': 'origin/main',
            'fullName': 'refs/remotes/origin/main',
            'kind': 'remote',
            'remote': 'origin',
            'isHead': false,
          },
        ],
      },
    ];
    final http = LanHttpClient();
    addTearDown(http.close);
    final git = GitClient(http, baseUrl);
    final commits = await git.commits('p1', worktreeId: 'wt-main');
    expect(lastPath, '/api/mobile/workbench/git/commits');
    expect(lastBody['projectId'], 'p1');
    expect(lastBody['worktreeId'], 'wt-main');
    expect(lastBody['limit'], 30);
    expect(commits, hasLength(1));
    final commit = commits.single;
    expect(commit.hash, 'a1b2c3d4e5f6');
    expect(commit.shortHash, 'a1b2c3d');
    expect(commit.summary, 'fix: mobile git history');
    expect(commit.authorName, '韩梅梅');
    expect(commit.authorEmail, 'han@example.com');
    expect(commit.parentHashes, ['fff0001']);
    expect(commit.refs, hasLength(2));
    expect(commit.refs.first.name, 'main');
    expect(commit.refs.first.kind, 'local');
    expect(commit.refs.first.isHead, isTrue);
    expect(commit.refs.last.remote, 'origin');
    expect(commit.refs.last.isHead, isFalse);
  });

  test(
    'commits parsing tolerates missing fields and a wrapped payload',
    () async {
      commitsResponse = {
        'commits': [
          {'hash': 'x'},
        ],
      };
      final http = LanHttpClient();
      addTearDown(http.close);
      final git = GitClient(http, baseUrl);
      final commits = await git.commits('p1');
      expect(lastBody['worktreeId'], isNull);
      expect(lastBody['limit'], 30);
      expect(commits, hasLength(1));
      final commit = commits.single;
      expect(commit.hash, 'x');
      expect(commit.shortHash, '');
      expect(commit.summary, '');
      expect(commit.authorName, '');
      expect(commit.parentHashes, isEmpty);
      expect(commit.refs, isEmpty);
    },
  );

  test('commits forwards a custom limit and empty payload', () async {
    commitsResponse = const [];
    final http = LanHttpClient();
    addTearDown(http.close);
    final git = GitClient(http, baseUrl);
    final commits = await git.commits('p1', worktreeId: null, limit: 5);
    expect(lastBody['limit'], 5);
    expect(lastBody['worktreeId'], isNull);
    expect(commits, isEmpty);
  });

  test('parses worktree git status leniently', () {
    final status = WorktreeGitStatus.of({
      'status': {
        'branch': 'feat/app',
        'changed': 2,
        'ahead': 1,
        'behind': 3,
        'conflicts': 0,
        'clean': false,
        'canPush': true,
      },
    });
    expect(status.present, isTrue);
    expect(status.branch, 'feat/app');
    expect(status.changed, 2);
    expect(status.ahead, 1);
    expect(status.behind, 3);
    expect(status.conflicts, 0);
    expect(status.clean, isFalse);
    expect(status.canPush, isTrue);

    final absent = WorktreeGitStatus.of({'id': 'wt-1'});
    expect(absent.present, isFalse);
    expect(absent.branch, isNull);
    expect(absent.clean, isTrue);
    expect(absent.canPush, isFalse);
  });

  test(
    'worktreeDisplayName prefers branch over name/id and label status is shared',
    () {
      // 对齐 web MobileWorktreeTabs label = branch ?? name（round4 起分支名优先）。
      expect(
        worktreeDisplayName({'id': 'w1', 'name': 'feat', 'branch': 'feat/app'}),
        'feat/app',
      );
      expect(worktreeDisplayName({'id': 'w1', 'name': 'feat'}), 'feat');
      expect(worktreeDisplayName({'id': 'w1'}), 'w1');
      expect(worktreeDisplayName(<String, dynamic>{}), '');
    },
  );

  test(
    'worktreeStatusLabel shares the conflict/dirty/clean wording across pages',
    () {
      // Git 页与 worktrees 页共用同一口径（N 处冲突 / N 处改动 / 干净）。
      expect(
        worktreeStatusLabel({
          'status': {'conflicts': 2, 'changed': 5, 'clean': false},
        }),
        '2 处冲突',
      );
      expect(
        worktreeStatusLabel({
          'status': {'changed': 3, 'clean': false},
        }),
        '3 处改动',
      );
      expect(
        worktreeStatusLabel({
          'status': {'changed': 0, 'clean': true},
        }),
        '干净',
      );
      // status 缺失按干净展示（宽容解析）。
      expect(worktreeStatusLabel({'id': 'wt-1'}), '干净');
    },
  );

  test('creates a worktree and commits with a message', () async {
    final http = LanHttpClient();
    addTearDown(http.close);
    final git = GitClient(http, baseUrl);
    final created = await git.create(projectId: 'p1', branchName: 'feat/app');
    expect(created['id'], 'wt-feat');
    expect(lastPath, '/api/mobile/workbench/worktrees/create');
    await git.commit(
      worktreeId: 'wt-feat',
      clientOperationId: 'op-1',
      message: 'fix: mobile git',
    );
    expect(lastPath, '/api/mobile/workbench/worktrees/commit');
    expect(lastBody['message'], 'fix: mobile git');
    expect(lastBody['clientOperationId'], 'op-1');
  });

  test('repairs a failed git hook on the existing route', () async {
    final http = LanHttpClient();
    addTearDown(http.close);
    final git = GitClient(http, baseUrl);
    await git.repairHookFailure(
      worktreeId: 'wt-feat',
      hookFailure: {'hook': 'commit-msg', 'output': 'blocked'},
    );
    expect(lastPath, '/api/mobile/workbench/worktrees/repair-hook-failure');
    expect(lastBody['worktreeId'], 'wt-feat');
  });
}
