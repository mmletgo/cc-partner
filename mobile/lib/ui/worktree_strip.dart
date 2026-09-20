import 'package:flutter/material.dart';

import '../git/client.dart';
import '../sessions/client.dart';

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

/// worktree 创建流程结果（共享 helper 的返回值）。
class WorktreeCreationResult {
  const WorktreeCreationResult({this.created, this.createError, this.sessionError});

  /// 创建成功的 worktree DTO（envelope value，供 shell 切换 active）。
  final Map<String, dynamic>? created;

  /// worktrees/create 失败原因（非空表示创建未成功，无副作用可重试）。
  final Object? createError;

  /// 绑定终端窗口创建失败原因（worktree 已保留，不回滚，对齐 web 行为）。
  final Object? sessionError;
}

/// Business Logic: 用户新建 worktree 后下一步就是进终端，所以创建成功要自动开绑定窗口；
/// 窗口创建失败时保留 worktree、只报错不回滚（对齐 web createWorktreeWithTerminalWindow）。
/// worktrees 页与切换条「+ 新建」共用同一执行通道，保证行为与文案一致。
/// Code Logic: worktrees/create → sessions/create（失败不回滚，记入 sessionError）→ 返回 DTO。
Future<WorktreeCreationResult> createWorktreeWithTerminalSession({
  required GitClient git,
  required SessionsClient sessions,
  required String projectId,
  required String branchName,
  Future<SessionSummary> Function(String projectId, String worktreeId)? onCreateSession,
}) async {
  Map<String, dynamic> created;
  try {
    created = Map<String, dynamic>.from(
      await git.create(projectId: projectId, branchName: branchName),
    );
  } catch (error) {
    return WorktreeCreationResult(createError: error);
  }
  Object? sessionError;
  final newId = created['id'] as String?;
  if (newId != null && newId.isNotEmpty) {
    try {
      final opener = onCreateSession;
      if (opener != null) {
        await opener(projectId, newId);
      } else {
        await sessions.create(projectId, worktreeId: newId);
      }
    } catch (error) {
      sessionError = error;
    }
  }
  return WorktreeCreationResult(created: created, sessionError: sessionError);
}

/// Business Logic: 移动端 worktree 切换条要对齐 web MobileWorktreeTabs——
/// chip 带状态点、分支名与主/linked 元信息，非主 chip 有 X 删除入口；
/// 条上要能直接新建 worktree（prefix/suffix inline 表单），mutation 结果未知时
/// 条上方出错误条并提供「重新对账」入口。
/// Code Logic: 纵向 Column = 可选错误条 + 水平 chip 列表（含尾部创建槽）；
/// 列表为空时渲染「暂无 worktree」占位（对齐 web worktrees.empty）；
/// onRemove 非空且非主 chip 时渲染尾部 X；onCreate 非空时渲染「+ 新建」创建槽。
class WorktreeStrip extends StatelessWidget {
  const WorktreeStrip({
    super.key,
    required this.worktrees,
    required this.activeId,
    required this.onSelect,
    this.onRemove,
    this.busy = false,
    this.onCreate,
    this.creating = false,
    this.mutationError,
    this.onRetryReconcile,
  });

  final List<Map<String, dynamic>> worktrees;
  final String? activeId;
  final ValueChanged<Map<String, dynamic>> onSelect;

  /// 非空时非主 chip 显示删除按钮；确认对话框由父层（workbench_home）负责。
  final ValueChanged<Map<String, dynamic>>? onRemove;

  /// 删除/切换/创建等操作进行中时禁用删除与创建入口。
  final bool busy;

  /// 非空时尾部展示「+ 新建」创建槽；回调入参为组合好的 `prefix/suffix` 分支名。
  final ValueChanged<String>? onCreate;

  /// 创建流程进行中（防重复提交，禁用表单）。
  final bool creating;

  /// mutation 结果未知等错误文案；非空时条上方渲染错误条（对齐 web MobileWorktreeTabs error）。
  final String? mutationError;

  /// 错误条上的「重新对账」入口（仅 unknown 相位由父层提供）。
  final VoidCallback? onRetryReconcile;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Column(
      mainAxisSize: MainAxisSize.min,
      children: [
        if (mutationError != null) _errorBanner(theme, mutationError!),
        SizedBox(
          height: 44,
          child: ListView(
            scrollDirection: Axis.horizontal,
            padding: const EdgeInsets.symmetric(horizontal: 8),
            children: [
              // 空列表占位（对齐 web MobileWorktreeTabs 的 worktrees.empty）：
              // 只剩创建槽时给出「暂无 worktree」提示，避免条上空白让人以为加载中。
              if (worktrees.isEmpty)
                Padding(
                  key: const Key('worktree-strip-empty'),
                  padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 12),
                  child: Text(
                    '暂无 worktree',
                    style: theme.textTheme.bodySmall
                        ?.copyWith(color: theme.colorScheme.onSurfaceVariant),
                  ),
                ),
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
              if (onCreate != null)
                Padding(
                  padding: const EdgeInsets.symmetric(horizontal: 4, vertical: 6),
                  child: _WorktreeCreateSlot(onCreate: onCreate!, busy: busy || creating),
                ),
            ],
          ),
        ),
      ],
    );
  }

  /// Business Logic: mutation 结果未知时用户必须能就地重新对账（对齐 web 条上错误条语义）。
  /// Code Logic: errorContainer 底色行 = 错误图标 + 文案 + 「重新对账」按钮。
  Widget _errorBanner(ThemeData theme, String message) {
    return Container(
      key: const Key('worktree-strip-error'),
      width: double.infinity,
      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 4),
      color: theme.colorScheme.errorContainer,
      child: Row(
        children: [
          Icon(Icons.error_outline, size: 16, color: theme.colorScheme.error),
          const SizedBox(width: 6),
          Expanded(
            child: Text(
              message,
              style: theme.textTheme.bodySmall,
              overflow: TextOverflow.ellipsis,
            ),
          ),
          if (onRetryReconcile != null)
            TextButton(
              key: const Key('worktree-retry-reconcile'),
              onPressed: onRetryReconcile,
              child: const Text('重新对账'),
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

/// 切换条尾部的创建槽：「+ 新建」chip ↔ 展开 inline 表单（前缀下拉 + `/` + 后缀输入）。
///
/// Business Logic（为什么需要）:
///   对齐 web MobileWorktreeTabs 条上 prefix/suffix 创建表单；前缀选项与 worktrees 页
///   创建表单一致（kWorktreeBranchPrefixes），组合逻辑复用 composeWorktreeBranchName。
///
/// Code Logic（做什么）:
///   局部 state 管理 open/prefix/suffix；确认时组合分支名回调 onCreate（空后缀禁用确认）；
///   busy 时禁用全部输入；取消收起表单。
class _WorktreeCreateSlot extends StatefulWidget {
  const _WorktreeCreateSlot({required this.onCreate, required this.busy});

  final ValueChanged<String> onCreate;
  final bool busy;

  @override
  State<_WorktreeCreateSlot> createState() => _WorktreeCreateSlotState();
}

class _WorktreeCreateSlotState extends State<_WorktreeCreateSlot> {
  bool _open = false;
  String _prefix = kDefaultWorktreeBranchPrefix;
  final TextEditingController _suffix = TextEditingController();

  @override
  void dispose() {
    _suffix.dispose();
    super.dispose();
  }

  /// Business Logic: 空后缀禁止创建（composeWorktreeBranchName 返回 null 的同一语义）。
  /// Code Logic: 组合成功即回调 onCreate 并收起表单清空输入；失败（空后缀）不动。
  void _confirm() {
    final branch = composeWorktreeBranchName(_prefix, _suffix.text);
    if (branch == null || widget.busy) {
      return;
    }
    setState(() {
      _open = false;
      _suffix.clear();
    });
    widget.onCreate(branch);
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    if (!_open) {
      return ActionChip(
        key: const Key('worktree-create'),
        avatar: const Icon(Icons.add, size: 16),
        label: const Text('新建'),
        onPressed: widget.busy
            ? null
            : () {
                setState(() => _open = true);
              },
      );
    }
    return Container(
      key: const Key('worktree-create-form'),
      padding: const EdgeInsets.symmetric(horizontal: 8),
      decoration: BoxDecoration(
        color: theme.colorScheme.surfaceContainerHighest,
        borderRadius: BorderRadius.circular(8),
      ),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          DropdownButton<String>(
            key: const Key('worktree-create-prefix'),
            value: _prefix,
            underline: const SizedBox.shrink(),
            items: [
              for (final prefix in kWorktreeBranchPrefixes)
                DropdownMenuItem(value: prefix, child: Text(prefix)),
            ],
            onChanged: widget.busy
                ? null
                : (value) {
                    if (value != null) {
                      setState(() => _prefix = value);
                    }
                  },
          ),
          const Padding(
            padding: EdgeInsets.symmetric(horizontal: 2),
            child: Text('/'),
          ),
          SizedBox(
            width: 96,
            child: TextField(
              key: const Key('worktree-create-suffix'),
              controller: _suffix,
              enabled: !widget.busy,
              onChanged: (_) => setState(() {}),
              style: theme.textTheme.bodySmall,
              decoration: const InputDecoration(hintText: 'my-task', isDense: true),
            ),
          ),
          const SizedBox(width: 4),
          TextButton(
            key: const Key('worktree-create-confirm'),
            onPressed:
                widget.busy || _suffix.text.trim().isEmpty ? null : _confirm,
            child: Text(widget.busy ? '创建中' : '确认'),
          ),
          TextButton(
            key: const Key('worktree-create-cancel'),
            onPressed: widget.busy
                ? null
                : () {
                    setState(() {
                      _open = false;
                      _suffix.clear();
                    });
                  },
            child: const Text('取消'),
          ),
        ],
      ),
    );
  }
}
