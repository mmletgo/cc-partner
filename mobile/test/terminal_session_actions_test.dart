import 'package:cc_partner_mobile/core/lan_http.dart';
import 'package:cc_partner_mobile/git/client.dart';
import 'package:cc_partner_mobile/prompts/client.dart';
import 'package:cc_partner_mobile/sessions/client.dart';
import 'package:cc_partner_mobile/terminal/git_actions.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  group('SessionSummary 宽容解析', () {
    test('supportsPanes/paneCount camelCase 读取，缺省回退默认值', () {
      final full = SessionSummary.fromJson({
        'id': 's1',
        'projectId': 'p1',
        'name': 'main',
        'status': 'running',
        'supportsPanes': true,
        'paneCount': 2,
      });
      expect(full.supportsPanes, isTrue);
      expect(full.paneCount, 2);

      final legacy = SessionSummary.fromJson({'id': 's2'});
      expect(legacy.supportsPanes, isFalse);
      expect(legacy.paneCount, 0);
    });

    test('snake_case 兜底解析', () {
      final s = SessionSummary.fromJson({
        'id': 's3',
        'project_id': 'p1',
        'worktree_id': 'w1',
        'supports_panes': true,
        'pane_count': 3,
      });
      expect(s.projectId, 'p1');
      expect(s.worktreeId, 'w1');
      expect(s.supportsPanes, isTrue);
      expect(s.paneCount, 3);
    });
  });

  group('closePane 返回解析', () {
    test('closedWindow true/false 与 snake 兜底', () {
      expect(ClosePaneResult.fromJson({'sessionId': 's1', 'closedWindow': true}).closedWindow,
          isTrue);
      expect(ClosePaneResult.fromJson({'session_id': 's2'}).closedWindow, isFalse);
    });
  });

  group('创建会话 body 语义', () {
    test('SessionsClient.create 带实测尺寸 / 未布局省略为 null', () async {
      final bodies = <Map<String, dynamic>>[];
      final client = SessionsClient(_RecordingHttp(bodies), 'http://test');
      await client.create('p1', worktreeId: 'w1', initialCols: 96, initialRows: 30);
      await client.create('p1');
      expect(bodies[0]['initialCols'], 96);
      expect(bodies[0]['initialRows'], 30);
      expect(bodies[0]['worktreeId'], 'w1');
      expect(bodies[1]['initialCols'], isNull);
      expect(bodies[1]['initialRows'], isNull);
    });
  });

  group('GitMutationOutcome 宽容解析', () {
    test('succeeded / unknown / failedHook 三形态', () {
      final ok = GitMutationOutcome.fromJson({
        'kind': 'succeeded',
        'value': {'id': 'w1'},
        'clientOperationId': 'op-1',
      });
      expect(ok.kind, GitMutationOutcomeKind.succeeded);
      expect(ok.clientOperationId, 'op-1');

      final unknown = GitMutationOutcome.fromJson({
        'kind': 'unknown',
        'clientOperationId': 'op-2',
        'transportClass': 'timeout',
      });
      expect(unknown.kind, GitMutationOutcomeKind.unknown);

      final hook = GitMutationOutcome.fromJson({
        'kind': 'failedHook',
        'clientOperationId': 'op-3',
        'hookFailure': {
          'stage': 'preCommit',
          'stdout': 'lint failed',
          'stderr': 'exit 1',
          'exitCode': 1,
        },
      });
      expect(hook.kind, GitMutationOutcomeKind.failedHook);
      expect(hook.hookFailure?.isPush, isFalse);
      expect(hook.hookFailure?.formattedOutput, 'lint failed\nexit 1');
      expect(hook.hookFailure?.exitCode, 1);
    });

    test('仅 hookFailure 载荷时按 failedHook 兜底；非法 kind 归 malformed', () {
      final fallback = GitMutationOutcome.fromJson({
        'hookFailure': {'stage': 'prePush', 'stdout': 'denied'},
      });
      expect(fallback.kind, GitMutationOutcomeKind.failedHook);
      expect(fallback.hookFailure?.isPush, isTrue);

      expect(GitMutationOutcome.fromJson({'kind': 'weird'}).kind,
          GitMutationOutcomeKind.malformed);
    });

    test('hook 输出拼接：两端都有拼接、只留非空端、全空为空', () {
      const view = HookFailureView(stdout: ' a ', stderr: ' b ');
      expect(view.formattedOutput, 'a\nb');
      expect(const HookFailureView(stdout: 'a').formattedOutput, 'a');
      expect(const HookFailureView(stderr: 'b').formattedOutput, 'b');
      expect(const HookFailureView().formattedOutput, '');
    });

    test('buildClientOperationId 可注入时钟与随机源', () {
      final id = buildClientOperationId('commit', nowMs: 1234);
      expect(id, startsWith('mobile-commit-1234-'));
      expect(buildClientOperationId('merge', nowMs: 1), isNot(buildClientOperationId('merge')));
    });
  });

  group('收藏 Prompt 标签派生与过滤', () {
    test('deriveTagsFromFavoritePrompts 去重排序，legacy tag 兼容', () {
      final prompts = [
        const FavoritePrompt(id: '1', title: 'a', content: '', tags: ['review', 'bug']),
        const FavoritePrompt(id: '2', title: 'b', content: '', tags: ['Review']),
        const FavoritePrompt(id: '3', title: 'c', content: '', tags: ['']),
      ];
      expect(deriveTagsFromFavoritePrompts(prompts), ['Review', 'bug', 'review']);
    });

    test('FavoritePrompt.fromJson：tags 缺失回退 legacy tag', () {
      expect(FavoritePrompt.fromJson({'id': '1', 'tag': 'x'}).tags, ['x']);
      expect(FavoritePrompt.fromJson({'id': '2', 'tags': ['a', 'b']}).tags, ['a', 'b']);
      expect(FavoritePrompt.fromJson({'id': '3'}).tags, isEmpty);
    });

    test('filterFavoritePrompts：标签 + title/content 子串（不区分大小写）', () {
      final prompts = [
        const FavoritePrompt(id: '1', title: 'Fix bug', content: 'please review', tags: ['bug']),
        const FavoritePrompt(id: '2', title: 'Deploy', content: 'release notes', tags: []),
      ];
      final bugOnly = filterFavoritePrompts(
        prompts,
        selectedTag: 'bug',
        allTagSentinel: '__all__',
        query: '',
      );
      expect(bugOnly.map((p) => p.id), ['1']);

      final matched = filterFavoritePrompts(
        prompts,
        selectedTag: '__all__',
        allTagSentinel: '__all__',
        query: 'RELEASE',
      );
      expect(matched.map((p) => p.id), ['2']);

      expect(
        filterFavoritePrompts(prompts, selectedTag: 'bug', allTagSentinel: '__all__', query: 'deploy'),
        isEmpty,
      );
    });
  });

  group('GitClient 既有签名调用', () {
    test('repairHookFailure body 原样回传 hookFailure', () async {
      final bodies = <Map<String, dynamic>>[];
      final client = GitClient(_RecordingHttp(bodies), 'http://test');
      await client.repairHookFailure(worktreeId: 'w1', hookFailure: {'stage': 'preCommit'});
      expect(bodies.single['hookFailure'], {'stage': 'preCommit'});
      expect(bodies.single['worktreeId'], 'w1');
    });
  });
}

/// 记录 POST body 的假 HTTP 通道（复用既有 client 测试模式）。
class _RecordingHttp extends LanHttpClient {
  _RecordingHttp(this.bodies) : super();

  final List<Map<String, dynamic>> bodies;

  @override
  Future<Map<String, dynamic>> postJson(
    String baseUrl,
    String path,
    Map<String, dynamic> body,
  ) async {
    bodies.add(body);
    return <String, dynamic>{};
  }
}
