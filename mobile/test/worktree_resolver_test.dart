import 'package:cc_partner_mobile/workbench/worktree.dart';
import 'package:test/test.dart';

void main() {
  const treesA = [
    {'id': 'wt-a-feat', 'isMain': false},
    {'id': 'wt-a-main', 'isMain': true},
  ];
  const treesB = [
    {'id': 'wt-b-main', 'isMain': true},
    {'id': 'wt-b-feat', 'isMain': false},
  ];

  test('new project cannot retain the previous worktree id', () {
    expect(
      resolveActiveWorktreeId(
        trees: treesB,
        previousId: 'wt-a-feat',
        projectChanged: true,
      ),
      'wt-b-main',
    );
    expect(
      resolveActiveWorktreeId(
        trees: [
          {'id': 'wt-a-feat', 'isMain': false},
          {'id': 'wt-b-main', 'isMain': true},
        ],
        previousId: 'wt-a-feat',
        projectChanged: true,
      ),
      'wt-b-main',
    );
  });

  test('same project keeps the previous id when it is still in the list', () {
    expect(
      resolveActiveWorktreeId(
        trees: treesA,
        previousId: 'wt-a-feat',
        projectChanged: false,
      ),
      'wt-a-feat',
    );
  });

  test('leaving a project clears the active worktree', () {
    expect(clearWorktreeOnLeaveProject(), isNull);
  });
}
