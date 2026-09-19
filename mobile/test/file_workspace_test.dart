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
}
