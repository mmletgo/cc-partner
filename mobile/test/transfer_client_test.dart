import 'package:cc_partner_mobile/transfer/client.dart';
import 'package:test/test.dart';

void main() {
  test('rejects host paths in task JSON', () {
    expect(
      () => TransferTask.fromJson({
        'id': 't1',
        'direction': 'Send',
        'status': 'completed',
        'filePath': '/Users/hans/secret.bin',
      }),
      throwsFormatException,
    );
  });

  test('accepts tasks without host paths', () {
    final task = TransferTask.fromJson({
      'id': 't1',
      'direction': 'Receive',
      'status': 'completed',
      'fileName': 'notes.txt',
    });
    expect(canDownload(task), isTrue);
  });

  test('upload starts immediately after pick', () {
    final plan = planUploadAfterPick(fileName: 'a.bin', size: 12);
    expect(plan.startImmediately, isTrue);
  });

  test('only completed receive or mobile-inbox send is downloadable', () {
    expect(
      canDownload(
        TransferTask(
          id: 's',
          direction: 'Send',
          status: 'completed',
          peer: 'other-pc',
        ),
      ),
      isFalse,
    );
    expect(
      canDownload(
        TransferTask(
          id: 'inbox',
          direction: 'Send',
          status: 'completed',
          peer: 'cc-partner-mobile-inbox',
        ),
      ),
      isTrue,
    );
  });

  test('host PC is first in the transfer target list', () {
    final ranked = rankTransferTargets([
      {'id': 'peer', 'isSelf': false, 'name': 'Other'},
      {'id': 'host', 'isSelf': true, 'name': 'This PC'},
    ]);
    expect(ranked.first['id'], 'host');
  });

  test('user can pick a non-host transfer target after host is pinned first', () {
    final ranked = rankTransferTargets([
      {'id': 'peer', 'isSelf': false, 'name': 'Other'},
      {'id': 'host', 'isSelf': true, 'name': 'This PC'},
    ]);
    expect(pickTransferTargetId(ranked), 'host');
    expect(pickTransferTargetId(ranked, selectedId: 'peer'), 'peer');
    expect(pickTransferTargetId(ranked, selectedId: 'gone'), 'host');
  });

  test('tasks group into active, needs-attention and completed in order', () {
    final groups = groupTransferTasks([
      TransferTask(id: 'a', direction: 'Send', status: 'pending'),
      TransferTask(id: 'b', direction: 'Send', status: 'transferring'),
      TransferTask(id: 'c', direction: 'Send', status: 'failed'),
      TransferTask(id: 'd', direction: 'Receive', status: 'cancelled'),
      TransferTask(id: 'e', direction: 'Receive', status: 'completed'),
    ]);
    expect(groups.active.map((t) => t.id), ['a', 'b']);
    expect(groups.needsAttention.map((t) => t.id), ['c', 'd']);
    expect(groups.completed.map((t) => t.id), ['e']);
  });

  test('unknown statuses fall into the completed group', () {
    final groups = groupTransferTasks([
      TransferTask(id: 'x', direction: 'Send', status: 'weird'),
    ]);
    expect(groups.completed.map((t) => t.id), ['x']);
    expect(groups.active, isEmpty);
    expect(groups.needsAttention, isEmpty);
  });

  test('task JSON parses recovery fields tolerantly', () {
    final task = TransferTask.fromJson({
      'id': 't1',
      'direction': 'send',
      'status': 'failed',
      'fileName': 'clip.bin',
      'progress': 0.5,
      'transferredBytes': 4096,
      'fileSize': 8192,
      'peerDeviceId': 'pc-a',
      'peerDeviceName': 'Hans Mac',
      'logicalTransferId': 'lt-1',
      'failure': {'stage': 'transfer', 'code': 'peerGone', 'retryable': false, 'message': 'x'},
    });
    expect(task.peer, 'pc-a');
    expect(task.peerDeviceName, 'Hans Mac');
    expect(task.progress, 0.5);
    expect(task.transferredBytes, 4096);
    expect(task.failureRetryable, isFalse);
    expect(task.logicalTransferId, 'lt-1');
    // 旧字段 `peer` 与缺省 failure 也能宽容解析。
    final legacy = TransferTask.fromJson({
      'id': 't2',
      'direction': 'Receive',
      'status': 'completed',
      'peer': 'inbox',
    });
    expect(legacy.peer, 'inbox');
    expect(legacy.failureRetryable, isNull);
    expect(legacy.progress, 0);
  });

  test('device supports resume only with transfer.resume.v1 capability', () {
    expect(
      deviceSupportsTransferResume({
        'id': 'a',
        'capabilities': ['transfer.resume.v1'],
      }),
      isTrue,
    );
    expect(
      deviceSupportsTransferResume({
        'id': 'a',
        'capabilities': <String>[],
      }),
      isFalse,
    );
    expect(deviceSupportsTransferResume({'id': 'a'}), isFalse);
  });

  test('peer resume judgement is fail-closed and trusts self devices', () {
    final devices = [
      {'id': 'host', 'isSelf': true},
      {
        'id': 'new-peer',
        'isSelf': false,
        'capabilities': ['transfer.resume.v1'],
      },
      {'id': 'old-peer', 'isSelf': false},
    ];
    TransferTask taskWithPeer(String id) =>
        TransferTask(id: 't', direction: 'send', status: 'failed', peer: id);
    // 本机目标视为支持；新 peer 带能力支持；旧 peer 与未知 peer fail-closed。
    expect(peerSupportsTransferResume(taskWithPeer('host'), devices), isTrue);
    expect(peerSupportsTransferResume(taskWithPeer('new-peer'), devices), isTrue);
    expect(peerSupportsTransferResume(taskWithPeer('old-peer'), devices), isFalse);
    expect(peerSupportsTransferResume(taskWithPeer('ghost'), devices), isFalse);
    expect(
      peerSupportsTransferResume(
        TransferTask(id: 't', direction: 'send', status: 'failed'),
        devices,
      ),
      isFalse,
    );
  });

  test('failed send with confirmed bytes prefers resume over retry', () {
    final resumable = TransferTask(
      id: 't1',
      direction: 'send',
      status: 'failed',
      transferredBytes: 1024,
    );
    expect(isTransferResumable(resumable, true), isTrue);
    expect(isTransferRetryable(resumable, true), isFalse);
    // progress 在 (0,1) 区间同样可续传。
    final progressOnly = TransferTask(
      id: 't2',
      direction: 'send',
      status: 'failed',
      progress: 0.5,
    );
    expect(isTransferResumable(progressOnly, true), isTrue);
    // 无已确认字节的失败只能重传。
    final freshFailure = TransferTask(id: 't3', direction: 'send', status: 'failed');
    expect(isTransferResumable(freshFailure, true), isFalse);
    expect(isTransferRetryable(freshFailure, true), isTrue);
    // cancelled 只能显式重传。
    final cancelled = TransferTask(id: 't4', direction: 'send', status: 'cancelled');
    expect(isTransferRetryable(cancelled, true), isTrue);
    // 旧 peer 无能力时全部回退重传判定（甚至直接不可恢复）。
    expect(isTransferResumable(resumable, false), isFalse);
    // 标记不可重试的失败两者都不可用。
    final fatal = TransferTask(
      id: 't5',
      direction: 'send',
      status: 'failed',
      transferredBytes: 10,
      failureRetryable: false,
    );
    expect(isTransferResumable(fatal, true), isFalse);
    expect(isTransferRetryable(fatal, true), isFalse);
    // 接收方向永远不恢复/重传。
    final receive = TransferTask(id: 't6', direction: 'receive', status: 'failed');
    expect(isTransferRetryable(receive, true), isFalse);
  });

  test('recovery is locked while a sibling attempt is active or reconciling', () {
    final failed = TransferTask(
      id: 'old',
      direction: 'send',
      status: 'failed',
      logicalTransferId: 'lt-1',
    );
    final activeSibling = TransferTask(
      id: 'new',
      direction: 'send',
      status: 'transferring',
      logicalTransferId: 'lt-1',
    );
    expect(isTransferRecoveryLocked(failed, [failed, activeSibling], {}), isTrue);
    // 对账中的 sibling 同样锁定。
    expect(
      isTransferRecoveryLocked(
        failed,
        [failed, activeSibling],
        {'new'},
      ),
      isTrue,
    );
    // 只有自己（failed 未对账）不锁。
    expect(isTransferRecoveryLocked(failed, [failed], {}), isFalse);
    // 不同 logical 互不影响。
    final unrelated = TransferTask(id: 'z', direction: 'send', status: 'transferring');
    expect(isTransferRecoveryLocked(failed, [failed, unrelated], {}), isFalse);
  });
}
