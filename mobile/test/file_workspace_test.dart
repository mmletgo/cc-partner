import 'package:cc_partner_mobile/files/workspace.dart';
import 'package:test/test.dart';

void main() {
  test('dirty guard blocks project/worktree switch', () {
    final files = FileWorkspaceController();
    files.markDirty(projectId: 'p1', worktreeId: 'wt1', path: 'a.rs');
    expect(
      files.shouldBlockContextSwitch(projectId: 'p2', worktreeId: 'wt1'),
      isTrue,
    );
    expect(
      files.shouldBlockContextSwitch(projectId: 'p1', worktreeId: 'wt2'),
      isTrue,
    );
    expect(
      files.shouldBlockContextSwitch(projectId: 'p1', worktreeId: 'wt1'),
      isFalse,
    );
    files.markClean();
    expect(
      files.shouldBlockContextSwitch(projectId: 'p2', worktreeId: 'wt1'),
      isFalse,
    );
  });

  test('detects preview kinds', () {
    expect(detectFileKind('a.rs'), FileKind.code);
    expect(detectFileKind('README.md'), FileKind.markdown);
    expect(detectFileKind('index.html'), FileKind.html);
    expect(detectFileKind('shot.png', mime: 'image/png'), FileKind.image);
    expect(detectFileKind('data.csv'), FileKind.csv);
    expect(detectFileKind('app.sqlite'), FileKind.sqlite);
  });

  test('markdown preview has source / render / split modes', () {
    expect(kMarkdownPreviewModes, ['source', 'render', 'split']);
  });

  test('sqlite preview selects a table then rows', () {
    final preview = SqlitePreviewState.fromOpen({
      'tables': ['users', 'orders'],
      'table': 'users',
      'rows': [
        {'id': 1},
      ],
    });
    expect(preview.tables, ['users', 'orders']);
    expect(preview.selectedTable, 'users');
    expect(preview.selectTable('orders').selectedTable, 'orders');
  });
}
