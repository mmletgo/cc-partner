import 'package:cc_partner_mobile/attention/filter.dart';
import 'package:test/test.dart';

void main() {
  test('hides tmux/dependency Inbox items', () {
    final items = [
      const AttentionItem(
        id: 'workbench:dependency:tmux',
        sourceKind: 'workbenchDependency',
        targetKind: 'settings',
      ),
      const AttentionItem(
        id: 'agent-1',
        sourceKind: 'agentNeedsInput',
        targetKind: 'agentSession',
        projectId: 'p1',
        sessionId: 's1',
      ),
    ];
    final visible = filterMobileInboxAttentionItems(items);
    expect(visible, hasLength(1));
    expect(visible.single.id, 'agent-1');
    expect(isMobileHiddenAttentionItem(items.first), isTrue);
  });

  test('navigate-only maps agentNeedsInput to the terminal panel', () {
    const item = AttentionItem(
      id: 'agent-1',
      sourceKind: 'agentNeedsInput',
      targetKind: 'agentSession',
      projectId: 'p1',
      sessionId: 's1',
    );
    final nav = navigateAttention(item);
    expect(nav.panel, 'terminal');
    expect(nav.sessionId, 's1');
  });

  test('orchestrator Inbox targets navigate to automation, not Inbox itself', () {
    const task = AttentionItem(
      id: 'task-1',
      sourceKind: 'orchestrator',
      targetKind: 'orchestratorTask',
      projectId: 'p1',
    );
    const outbox = AttentionItem(
      id: 'outbox-1',
      sourceKind: 'orchestrator',
      targetKind: 'remoteOutbox',
      projectId: 'p1',
    );
    const experiment = AttentionItem(
      id: 'exp-1',
      sourceKind: 'orchestrator',
      targetKind: 'experiment',
      projectId: 'p1',
    );
    expect(navigateAttention(task).panel, 'automation');
    expect(navigateAttention(outbox).panel, 'automation');
    expect(navigateAttention(experiment).panel, 'automation');
  });
}
