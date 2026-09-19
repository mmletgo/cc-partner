import 'package:flutter/material.dart';

class WorktreeStrip extends StatelessWidget {
  const WorktreeStrip({
    super.key,
    required this.worktrees,
    required this.activeId,
    required this.onSelect,
  });

  final List<Map<String, dynamic>> worktrees;
  final String? activeId;
  final ValueChanged<Map<String, dynamic>> onSelect;

  @override
  Widget build(BuildContext context) {
    return SizedBox(
      height: 44,
      child: ListView(
        scrollDirection: Axis.horizontal,
        padding: const EdgeInsets.symmetric(horizontal: 8),
        children: [
          for (final tree in worktrees)
            Padding(
              padding: const EdgeInsets.symmetric(horizontal: 4, vertical: 6),
              child: ChoiceChip(
                key: Key('worktree-${tree['id']}'),
                label: Text(
                  tree['name'] as String? ??
                      tree['branch'] as String? ??
                      tree['id'] as String? ??
                      '',
                ),
                selected: tree['id'] == activeId,
                onSelected: (_) => onSelect(tree),
              ),
            ),
        ],
      ),
    );
  }
}
