import 'package:flutter/material.dart';

import '../git/client.dart';

/// worktree 状态三色（对齐 web worktreeStatusTone：neutral/warning/danger）。
enum WorktreeStripTone { clean, dirty, conflict }

/// Business Logic: 切换条上的圆点要让用户一眼分辨工作区是否干净/有改动/冲突。
/// Code Logic: conflicts>0 → conflict；非 clean → dirty；否则 clean（与 web 同口径）。
WorktreeStripTone worktreeStripToneOf(Map<String, dynamic> tree) {
  final status = WorktreeGitStatus.of(tree);
  if (status.conflicts > 0) {
    return WorktreeStripTone.conflict;
  }
  if (!status.clean || status.changed > 0) {
    return WorktreeStripTone.dirty;
  }
  return WorktreeStripTone.clean;
}

/// Business Logic: 禁止硬编码颜色，三色必须来自 ThemeData 语义色（深浅色主题都能读）。
/// Code Logic: clean → primary，dirty → tertiary，conflict → error。
Color worktreeStripToneColor(ThemeData theme, WorktreeStripTone tone) {
  switch (tone) {
    case WorktreeStripTone.clean:
      return theme.colorScheme.primary;
    case WorktreeStripTone.dirty:
      return theme.colorScheme.tertiary;
    case WorktreeStripTone.conflict:
      return theme.colorScheme.error;
  }
}

/// Business Logic: 移动端 worktree 切换条要对齐 web MobileWorktreeTabs——
/// chip 带状态点、分支名与主/linked 元信息，非主 chip 有 X 删除入口。
/// Code Logic: 水平 chip 列表；onRemove 非空且非主 chip 时渲染尾部 X；主 chip 无删除。
class WorktreeStrip extends StatelessWidget {
  const WorktreeStrip({
    super.key,
    required this.worktrees,
    required this.activeId,
    required this.onSelect,
    this.onRemove,
    this.busy = false,
  });

  final List<Map<String, dynamic>> worktrees;
  final String? activeId;
  final ValueChanged<Map<String, dynamic>> onSelect;

  /// 非空时非主 chip 显示删除按钮；确认对话框由父层（workbench_home）负责。
  final ValueChanged<Map<String, dynamic>>? onRemove;

  /// 删除/切换等操作进行中时禁用删除入口。
  final bool busy;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return SizedBox(
      height: 44,
      child: ListView(
        scrollDirection: Axis.horizontal,
        padding: const EdgeInsets.symmetric(horizontal: 8),
        children: [
          for (final tree in worktrees)
            Padding(
              padding: const EdgeInsets.symmetric(horizontal: 4, vertical: 6),
              child: Row(
                mainAxisSize: MainAxisSize.min,
                children: [
                  ChoiceChip(
                    key: Key('worktree-${tree['id']}'),
                    label: Row(
                      mainAxisSize: MainAxisSize.min,
                      children: [
                        _dot(theme, tree),
                        const SizedBox(width: 6),
                        Text(worktreeDisplayName(tree)),
                        const SizedBox(width: 4),
                        Text(
                          tree['isMain'] == true ? '主' : 'worktree',
                          style: theme.textTheme.labelSmall
                              ?.copyWith(color: theme.colorScheme.onSurfaceVariant),
                        ),
                      ],
                    ),
                    selected: tree['id'] == activeId,
                    onSelected: (_) => onSelect(tree),
                  ),
                  if (onRemove != null && tree['isMain'] != true) ...[
                    const SizedBox(width: 2),
                    _removeButton(context, theme, tree),
                  ],
                ],
              ),
            ),
        ],
      ),
    );
  }

  /// Business Logic: 状态点只表达视觉信号，不参与点击。
  /// Code Logic: 8px 圆点按 tone 取主题色。
  Widget _dot(ThemeData theme, Map<String, dynamic> tree) {
    return Container(
      width: 8,
      height: 8,
      decoration: BoxDecoration(
        color: worktreeStripToneColor(theme, worktreeStripToneOf(tree)),
        shape: BoxShape.circle,
      ),
    );
  }

  /// Business Logic: 非主 worktree 要能就地清理，busy 时禁用防止重复删除。
  /// Code Logic: 24px 紧凑 IconButton，key 带 worktree id 供测试与读屏定位。
  Widget _removeButton(BuildContext context, ThemeData theme, Map<String, dynamic> tree) {
    final id = tree['id'] as String? ?? '';
    return SizedBox(
      width: 24,
      height: 24,
      child: IconButton(
        key: Key('worktree-remove-$id'),
        padding: EdgeInsets.zero,
        iconSize: 14,
        tooltip: '移除',
        color: theme.colorScheme.onSurfaceVariant,
        disabledColor: theme.colorScheme.outline,
        icon: const Icon(Icons.close),
        onPressed: busy ? null : () => onRemove!(tree),
      ),
    );
  }
}
