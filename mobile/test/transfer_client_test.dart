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
}
