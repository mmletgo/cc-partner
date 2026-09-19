import 'dart:async';
import 'dart:io';

import 'package:cc_partner_mobile/core/lan_http.dart';
import 'package:cc_partner_mobile/git/mutation.dart';
import 'package:test/test.dart';

void main() {
  group('相位推进纯函数', () {
    test('unknown/reconciling 期间复用同一 operationId，其余铸造新 id', () {
      expect(
        pickGitMutationOperationId(GitMutationPhase.unknown, 'op-1', 'op-2'),
        'op-1',
      );
      expect(
        pickGitMutationOperationId(GitMutationPhase.reconciling, 'op-1', 'op-2'),
        'op-1',
      );
      expect(
        pickGitMutationOperationId(GitMutationPhase.idle, 'op-1', 'op-2'),
        'op-2',
      );
      expect(
        pickGitMutationOperationId(GitMutationPhase.busy, null, 'op-2'),
        'op-2',
      );
    });

    test('busy/reconciling/unknown 锁定动作，idle 不锁', () {
      expect(isGitMutationActionLocked(GitMutationPhase.idle), isFalse);
      expect(isGitMutationActionLocked(GitMutationPhase.busy), isTrue);
      expect(isGitMutationActionLocked(GitMutationPhase.reconciling), isTrue);
      expect(isGitMutationActionLocked(GitMutationPhase.unknown), isTrue);
    });

    test('ledger 终态映射，中间态与未知值返回 null', () {
      expect(
        ledgerTerminalState({'state': 'succeeded'}),
        GitMutationReconcile.confirmedSucceeded,
      );
      expect(
        ledgerTerminalState({'state': 'failed'}),
        GitMutationReconcile.confirmedFailed,
      );
      expect(ledgerTerminalState({'state': 'running'}), isNull);
      expect(ledgerTerminalState({'state': 'claimed'}), isNull);
      expect(ledgerTerminalState(null), isNull);
    });

    test('kind 解析宽容：未知值回 null', () {
      expect(parseGitMutationKind('merge'), GitMutationKind.merge);
      expect(parseGitMutationKind('collectMerge'), GitMutationKind.collectMerge);
      expect(parseGitMutationKind('nope'), isNull);
      expect(parseGitMutationKind(null), isNull);
    });
  });

  group('reconcileGitMutation 对账矩阵', () {
    test('ledger 终态优先：succeeded/failed 直接裁决', () {
      expect(
        reconcileGitMutation(
          ledger: {
            'state': 'succeeded',
            'intent': {'kind': 'remove', 'worktreeId': 'wt-1'},
          },
          worktrees: const [
            {'id': 'wt-1'},
          ],
        ),
        GitMutationReconcile.confirmedSucceeded,
      );
      expect(
        reconcileGitMutation(
          ledger: {
            'state': 'failed',
            'intent': {'kind': 'remove', 'worktreeId': 'wt-1'},
          },
          worktrees: const [],
        ),
        GitMutationReconcile.confirmedFailed,
      );
    });

    test('ledger 缺失或 intent 缺失保持 unknown', () {
      expect(
        reconcileGitMutation(ledger: null, worktrees: const []),
        GitMutationReconcile.unknown,
      );
      expect(
        reconcileGitMutation(
          ledger: {'state': 'running'},
          worktrees: const [],
        ),
        GitMutationReconcile.unknown,
      );
    });

    test('remove：worktree 已不在列表 → 成功；仍在 → unknown', () {
      Map<String, dynamic> ledger({required bool present}) => {
            'state': 'running',
            'intent': {
              'kind': 'remove',
              'worktreeId': 'wt-1',
            },
          };
      expect(
        reconcileGitMutation(
          ledger: ledger(present: false),
          worktrees: const [
            {'id': 'wt-other'},
          ],
        ),
        GitMutationReconcile.confirmedSucceeded,
      );
      expect(
        reconcileGitMutation(
          ledger: ledger(present: true),
          worktrees: const [
            {'id': 'wt-1'},
          ],
        ),
        GitMutationReconcile.unknown,
      );
    });

    test('merge：源消失且主分支包含源 head → 成功；其余保持 unknown', () {
      Map<String, dynamic> ledger() => {
            'state': 'running',
            'intent': {
              'kind': 'merge',
              'sourceWorktreeId': 'wt-1',
              'sourceHead': 'abc1234',
            },
          };
      expect(
        reconcileGitMutation(
          ledger: ledger(),
          worktrees: const [
            {'id': 'wt-main'},
          ],
          mainCommitHashes: ['abc1234', 'def5678'],
        ),
        GitMutationReconcile.confirmedSucceeded,
      );
      // 主分支提交缺失时不得猜成功。
      expect(
        reconcileGitMutation(
          ledger: ledger(),
          worktrees: const [
            {'id': 'wt-main'},
          ],
        ),
        GitMutationReconcile.unknown,
      );
      // 源 worktree 还在：未生效。
      expect(
        reconcileGitMutation(
          ledger: ledger(),
          worktrees: const [
            {'id': 'wt-main'},
            {'id': 'wt-1'},
          ],
          mainCommitHashes: ['abc1234'],
        ),
        GitMutationReconcile.unknown,
      );
    });

    test('collectMerge：主分支包含全部源 head → 成功；部分包含保持 unknown', () {
      Map<String, dynamic> ledger() => {
            'state': 'running',
            'intent': {
              'kind': 'collectMerge',
              'sources': [
                {'name': 'a', 'oid': 'aaa111'},
                {'name': 'b', 'oid': 'bbb222'},
              ],
            },
          };
      expect(
        reconcileGitMutation(
          ledger: ledger(),
          worktrees: const [
            {'id': 'wt-main'},
          ],
          mainCommitHashes: ['aaa111', 'bbb222'],
        ),
        GitMutationReconcile.confirmedSucceeded,
      );
      expect(
        reconcileGitMutation(
          ledger: ledger(),
          worktrees: const [
            {'id': 'wt-main'},
          ],
          mainCommitHashes: ['aaa111'],
        ),
        GitMutationReconcile.unknown,
      );
    });

    test('commit/push/pull 无 ledger 终态时保持 unknown（不猜成功）', () {
      for (final kind in ['commit', 'push', 'pull']) {
        expect(
          reconcileGitMutation(
            ledger: {
              'state': 'running',
              'intent': {'kind': kind},
            },
            worktrees: const [
              {'id': 'wt-1'},
            ],
          ),
          GitMutationReconcile.unknown,
        );
      }
    });
  });

  group('GitMutationTracker 状态机', () {
    test('begin → busy；unknown → 锁定；终态解锁并清 id', () {
      final tracker = GitMutationTracker();
      expect(tracker.phase, GitMutationPhase.idle);
      final opId = tracker.begin(
        kind: GitMutationKind.push,
        worktreeId: 'wt-1',
        nextOperationId: 'op-1',
      );
      expect(opId, 'op-1');
      expect(tracker.phase, GitMutationPhase.busy);
      expect(tracker.actionLocked, isTrue);

      tracker.markUnknown();
      expect(tracker.phase, GitMutationPhase.unknown);
      expect(tracker.operationId, 'op-1');
      expect(tracker.unknownKind, GitMutationKind.push);

      // unknown 里再 begin：复用同一 id，不盲重放。
      final reused = tracker.begin(
        kind: GitMutationKind.push,
        worktreeId: 'wt-1',
        nextOperationId: 'op-2',
      );
      expect(reused, 'op-1');

      tracker.beginReconcile();
      expect(tracker.phase, GitMutationPhase.reconciling);

      tracker.settleReconcile(GitMutationReconcile.unknown);
      expect(tracker.phase, GitMutationPhase.unknown);

      tracker.settleReconcile(GitMutationReconcile.confirmedSucceeded);
      expect(tracker.phase, GitMutationPhase.idle);
      expect(tracker.operationId, isNull);
      expect(tracker.unknownKind, isNull);
      expect(tracker.actionLocked, isFalse);
    });

    test('确定性失败 markIdle 立即解锁；reset 清空上下文', () {
      final tracker = GitMutationTracker();
      tracker.begin(
        kind: GitMutationKind.merge,
        worktreeId: 'wt-1',
        nextOperationId: 'op-1',
      );
      tracker.markIdle();
      expect(tracker.phase, GitMutationPhase.idle);
      tracker.begin(
        kind: GitMutationKind.remove,
        worktreeId: 'wt-2',
        nextOperationId: 'op-2',
      );
      tracker.markUnknown();
      tracker.reset();
      expect(tracker.phase, GitMutationPhase.idle);
      expect(tracker.unknownKind, isNull);
    });
  });

  group('envelope 与传输异常判定', () {
    test('envelope 解析 succeeded / unknown / failedHook / 空形态', () {
      final ok = GitMutationEnvelope.from({
        'kind': 'succeeded',
        'value': {'id': 'wt-1'},
        'clientOperationId': 'op-1',
      });
      expect(ok.succeeded, isTrue);
      expect(ok.value?['id'], 'wt-1');

      final unknown = GitMutationEnvelope.from({
        'kind': 'unknown',
        'clientOperationId': 'op-2',
        'transportClass': 'timeout',
      });
      expect(unknown.unknown, isTrue);
      expect(unknown.clientOperationId, 'op-2');

      final hook = GitMutationEnvelope.from({
        'kind': 'failedHook',
        'clientOperationId': 'op-3',
        'hookFailure': {'output': 'blocked'},
      });
      expect(hook.failedHook, isTrue);
      expect(hook.hookFailure?['output'], 'blocked');

      expect(GitMutationEnvelope.from(null).kind, '');
      expect(GitMutationEnvelope.from('nope').succeeded, isFalse);
    });

    test('SocketException/Timeout 视为 unknown，服务器应答错误是确定失败', () {
      expect(isTransportUnknownError(const SocketException('reset')), isTrue);
      expect(isTransportUnknownError(TimeoutException('t')), isTrue);
      expect(
        isTransportUnknownError(LanHttpException(500, 'boom')),
        isFalse,
      );
      expect(isTransportUnknownError(StateError('x')), isFalse);
    });
  });
}
