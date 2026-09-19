import 'package:cc_partner_mobile/git/project_sync.dart';
import 'package:test/test.dart';

Map<String, dynamic> project(
  String id, {
  String? fingerprint,
  String? deviceId,
  String? deviceName,
}) =>
    {
      'id': id,
      'name': id,
      if (fingerprint != null) 'gitRemoteFingerprint': fingerprint,
      if (deviceId != null) 'deviceId': deviceId,
      if (deviceName != null) 'deviceName': deviceName,
    };

void main() {
  test('projectGroupKey：非空 fingerprint 用 fp:，否则 id:', () {
    expect(
      projectGroupKey(project('p1', fingerprint: ' github.com/a/b ')),
      'fp:github.com/a/b',
    );
    expect(projectGroupKey(project('p2')), 'id:p2');
    expect(projectGroupKey(project('p3', fingerprint: '')), 'id:p3');
  });

  test('siblingProjectsOnOtherDevices 只保留异设备的同 fingerprint 项目', () {
    final current = project('p1', fingerprint: 'fp-1', deviceId: 'dev-a');
    final projects = [
      current,
      project('p2', fingerprint: 'fp-1', deviceId: 'dev-b', deviceName: '书房'),
      project('p3', fingerprint: 'fp-1', deviceId: 'dev-a'),
      project('p4', fingerprint: 'fp-2', deviceId: 'dev-b'),
      project('p5', deviceId: 'dev-b'),
    ];
    final siblings = siblingProjectsOnOtherDevices(current, projects);
    expect(siblings.map((p) => p['id']), ['p2']);
    // 无 fingerprint 不参与同步。
    expect(siblingProjectsOnOtherDevices(project('p5', deviceId: 'dev-b'), projects), isEmpty);
    expect(siblingProjectsOnOtherDevices(null, projects), isEmpty);
  });

  test('pickMainWorktree 返回 isMain 首项，缺失回 null', () {
    expect(
      pickMainWorktree([
        {'id': 'wt-1', 'isMain': false},
        {'id': 'wt-main', 'isMain': true},
      ])?['id'],
      'wt-main',
    );
    expect(pickMainWorktree(const []), isNull);
    expect(
      pickMainWorktree([
        {'id': 'wt-1'},
      ]),
      isNull,
    );
  });

  test('canSyncProjectMain 门控：兄弟数量/可推送/busy/锁定', () {
    final main = {
      'id': 'wt-main',
      'isMain': true,
      'branch': 'main',
      'status': {'canPush': true},
    };
    final noPush = {
      'id': 'wt-main',
      'isMain': true,
      'branch': 'main',
      'status': {'canPush': false},
    };
    expect(
      canSyncProjectMain(
        mainWorktree: main,
        siblingCount: 2,
        busy: false,
        actionLocked: false,
      ),
      isTrue,
    );
    expect(
      canSyncProjectMain(
        mainWorktree: main,
        siblingCount: 0,
        busy: false,
        actionLocked: false,
      ),
      isFalse,
    );
    expect(
      canSyncProjectMain(
        mainWorktree: noPush,
        siblingCount: 2,
        busy: false,
        actionLocked: false,
      ),
      isFalse,
    );
    expect(
      canSyncProjectMain(
        mainWorktree: main,
        siblingCount: 2,
        busy: true,
        actionLocked: false,
      ),
      isFalse,
    );
    expect(
      canSyncProjectMain(
        mainWorktree: main,
        siblingCount: 2,
        busy: false,
        actionLocked: true,
      ),
      isFalse,
    );
    expect(
      canSyncProjectMain(
        mainWorktree: null,
        siblingCount: 2,
        busy: false,
        actionLocked: false,
      ),
      isFalse,
    );
  });

  test('syncSummaryText 区分全量成功与部分失败', () {
    expect(
      syncSummaryText(['书房', '客厅'], []),
      '已推送主分支，并在 书房 · 客厅 上拉取',
    );
    expect(
      syncSummaryText([], []),
      '已推送主分支，并在 无 上拉取',
    );
    expect(
      syncSummaryText(['书房'], ['客厅: 拉取失败']),
      '已推送主分支。已拉取：书房。失败：客厅: 拉取失败',
    );
  });
}
