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

  test('phase parses known values and keeps unknown or missing as null', () {
    final task = TransferTask.fromJson({
      'id': 't1',
      'direction': 'send',
      'status': 'transferring',
      'phase': 'connecting',
    });
    expect(task.phase, 'connecting');
    // 未知 phase 宽容保留 null，不抛错。
    final unknown = TransferTask.fromJson({
      'id': 't2',
      'direction': 'send',
      'status': 'transferring',
      'phase': 'warp-speed',
    });
    expect(unknown.phase, isNull);
    // 缺 phase 的旧后端任务同为 null。
    final legacy = TransferTask.fromJson({
      'id': 't3',
      'direction': 'send',
      'status': 'failed',
    });
    expect(legacy.phase, isNull);
  });

  test('task JSON parses failure message and top-level error message', () {
    final task = TransferTask.fromJson({
      'id': 't1',
      'direction': 'send',
      'status': 'failed',
      'failure': {'code': 'peerGone', 'message': 'peer went offline', 'retryable': true},
    });
    expect(task.failureMessage, 'peer went offline');
    expect(task.errorMessage, isNull);
    final fallback = TransferTask.fromJson({
      'id': 't2',
      'direction': 'send',
      'status': 'failed',
      'errorMessage': 'host unreachable',
    });
    expect(fallback.failureMessage, isNull);
    expect(fallback.errorMessage, 'host unreachable');
  });

  test('active phase keeps a task in the active group and attempt-active', () {
    // status 已到 completed 但 phase 仍在传输链路 → 视为 active。
    final inFlight = TransferTask(
      id: 'a',
      direction: 'send',
      status: 'completed',
      phase: 'transferring',
    );
    expect(classifyTransferGroup(inFlight), 'active');
    expect(isTransferAttemptActive(inFlight), isTrue);
    // 其余活跃 phase 同样生效。
    for (final phase in ['queued', 'connecting', 'finalizing']) {
      expect(
        classifyTransferGroup(
          TransferTask(id: 'p', direction: 'send', status: 'completed', phase: phase),
        ),
        'active',
        reason: 'phase $phase should stay active',
      );
    }
    // 终态 phase 不影响原分组。
    final done = TransferTask(
      id: 'b',
      direction: 'send',
      status: 'completed',
      phase: 'completed',
    );
    expect(classifyTransferGroup(done), 'completed');
    expect(isTransferAttemptActive(done), isFalse);
    // 无 phase 的行为保持不变。
    expect(
      classifyTransferGroup(TransferTask(id: 'c', direction: 'send', status: 'failed')),
      'needsAttention',
    );
    expect(isTransferAttemptActive(done), isFalse);
  });

  test('reconciling tasks group into needs-attention regardless of status', () {
    final groups = groupTransferTasks(
      [
        TransferTask(id: 'r', direction: 'send', status: 'completed'),
        TransferTask(id: 'ok', direction: 'send', status: 'completed'),
      ],
      reconcilingIds: {'r'},
    );
    expect(groups.needsAttention.map((t) => t.id), ['r']);
    expect(groups.completed.map((t) => t.id), ['ok']);
    // 单任务口径同样生效。
    expect(
      classifyTransferGroup(
        TransferTask(id: 'r2', direction: 'send', status: 'transferring'),
        reconciling: true,
      ),
      'needsAttention',
    );
  });

  test('peer display text prefers the mobile-inbox label over device name', () {
    expect(
      transferPeerDisplayText(
        TransferTask(id: 't', direction: 'send', status: 'completed', peer: mobileInboxDeviceId),
      ),
      '手机',
    );
    expect(
      transferPeerDisplayText(
        TransferTask(
          id: 't',
          direction: 'send',
          status: 'failed',
          peer: 'pc-1',
          peerDeviceName: 'Hans Mac',
        ),
      ),
      'Hans Mac',
    );
    expect(
      transferPeerDisplayText(TransferTask(id: 't', direction: 'send', status: 'failed')),
      isNull,
    );
  });

  test('phase label maps known phases to Chinese copy', () {
    expect(transferPhaseLabel('queued'), '排队中');
    expect(transferPhaseLabel('connecting'), '连接中');
    expect(transferPhaseLabel('transferring'), '传输中');
    expect(transferPhaseLabel('finalizing'), '收尾中');
    expect(transferPhaseLabel('completed'), '已完成');
    expect(transferPhaseLabel(null), isNull);
    expect(transferPhaseLabel('warp-speed'), isNull);
  });
}
