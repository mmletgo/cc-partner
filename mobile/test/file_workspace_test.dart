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

  test('乐观锁基线优先读 baseHash（后端真实字段），兼容回退 hash', () {
    expect(fileTextBaseHash({'baseHash': 'b1', 'hash': 'h1'}), 'b1');
    expect(fileTextBaseHash({'baseHash': 'b1'}), 'b1');
    expect(fileTextBaseHash({'hash': 'h1'}), 'h1');
    expect(fileTextBaseHash({'baseHash': ''}), '');
    expect(fileTextBaseHash(const {}), '');
  });

  test('CSV 表头优先读 columns（后端真实字段），回退 headers，缺省占位', () {
    expect(
      csvPreviewColumns({'columns': ['a', 'b'], 'headers': ['x']}),
      ['a', 'b'],
    );
    expect(
      csvPreviewColumns({'headers': ['x', 'y']}),
      ['x', 'y'],
    );
    expect(csvPreviewColumns({'columns': <String>[]}), ['col']);
    expect(csvPreviewColumns(const {}), ['col']);
  });
}
