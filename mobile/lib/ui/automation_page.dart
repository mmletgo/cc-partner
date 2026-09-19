import 'package:flutter/material.dart';

import '../address_book/book.dart';
import '../automation/client.dart';
import '../core/lan_http.dart';
import '../projects/client.dart';
import '../transfer/api.dart';

/// 泳道中文文案（对齐 web zh locale 的 workflow 标签）。
const Map<String, String> _workflowLabels = {
  'backlog': 'Backlog',
  'todo': 'Todo',
  'inProgress': 'In Progress',
  'humanReview': 'Human Review',
  'rework': 'Rework',
  'merging': 'Merging',
  'done': 'Done',
  'canceled': 'Canceled',
};

/// Outbox 状态中文文案（待发送/发送中/已同步/发送失败/已放弃）。
const Map<String, String> _pendingStatusLabels = {
  'pending': '待发送',
  'sending': '发送中',
  'mirrored': '已同步',
  'failed': '发送失败',
  'discarded': '已放弃',
};

/// Evidence kind 中文文案（未知 kind 回落到「证据」）。
const Map<String, String> _evidenceKindLabels = {
  'developmentAttempt': '开发尝试',
  'verificationOutput': '验证输出',
  'verificationReview': '验证器复核',
  'repairPrompt': '修复指令',
  'remoteOutbox': '远端 outbox',
  'delivery': '自动交付',
  'generic': '证据',
};

/// 创建动作 → 成功提示文案。
const Map<String, String> _createActionSuccessLabels = {
  'backlog': '任务已创建到 Backlog',
  'todo': '任务已创建到 Todo',
  'start': '任务已创建并启动',
};

/// runtime snapshot remoteStatus → 中文状态文案。
String _runtimeStatusLabel(Map<String, dynamic>? snapshot) {
  final status = snapshot?['remoteStatus'] as String?;
  switch (status) {
    case 'local':
      return '本机';
    case 'live':
      return '在线';
    case 'offline':
      return '离线';
    case 'unsupported':
      return '对端不支持';
    case 'unavailable':
      return '暂不可用';
    default:
      return '状态未知';
  }
}

/// Evidence 时间格式化：解析失败时原样返回，不让详情崩溃。
String _formatAutomationTimestamp(String value) {
  final date = DateTime.tryParse(value);
  if (date == null) return value;
  return '${date.toLocal()}'.split('.').first;
}

class AutomationPage extends StatefulWidget {
  const AutomationPage({
    super.key,
    required this.book,
    required this.http,
    required this.project,
    @visibleForTesting AutomationClient? clientOverride,
  }) : _clientOverride = clientOverride;

  final AddressBook book;
  final LanHttpClient http;
  final ProjectSummary project;

  /// 仅测试注入：替换内部 Orchestrator HTTP 客户端。
  final AutomationClient? _clientOverride;

  @override
  State<AutomationPage> createState() => _AutomationPageState();
}

class _AutomationPageState extends State<AutomationPage> {
  late final AutomationClient _client;
  List<Map<String, dynamic>> _views = [];
  List<Map<String, dynamic>> _experiments = [];
  Map<String, dynamic>? _snapshot;
  String? _error;
  bool _loading = true;

  /// 主列表请求序号守卫：过期响应直接丢弃。
  int _requestSeq = 0;

  String? _selectedTaskId;
  List<Map<String, dynamic>> _evidence = [];
  bool _evidenceLoading = false;
  String? _evidenceError;
  final Set<String> _expandedEvidenceIds = {};

  /// Evidence 请求序号守卫：切换任务后过期响应不覆盖当前详情。
  int _evidenceSeq = 0;

  String? _outboxActionId;

  final _title = TextEditingController();
  final _goal = TextEditingController();
  final _acceptance = TextEditingController();
  final _prompt = TextEditingController();
  String _createAction = 'backlog';
  bool _creating = false;
  bool _completing = false;

  @override
  void initState() {
    super.initState();
    _client = widget._clientOverride ??
        AutomationClient(widget.http, widget.book.active!.baseUrl);
    _reload();
  }

  @override
  void dispose() {
    _title.dispose();
    _goal.dispose();
    _acceptance.dispose();
    _prompt.dispose();
    super.dispose();
  }

  /// 拉取泳道视图 / 实验组 / runtime 快照；请求序号过期则丢弃整个响应。
  Future<void> _reload() async {
    final seq = ++_requestSeq;
    setState(() {
      _loading = true;
      _error = null;
    });
    try {
      final results = await Future.wait<dynamic>([
        _client.listViews(widget.project.id),
        _client
            .listExperiments(widget.project.id)
            .catchError((_) => <Map<String, dynamic>>[]),
        _client
            .runtimeSnapshot(widget.project.id)
            .catchError((_) => <String, dynamic>{}),
      ]);
      if (!mounted || seq != _requestSeq) return;
      final views = results[0] as List<Map<String, dynamic>>;
      final stillSelected = automationSplitViews(views).tasks.any(
            (view) => automationTaskOfView(view)?['id'] == _selectedTaskId,
          );
      setState(() {
        _views = views;
        _experiments = results[1] as List<Map<String, dynamic>>;
        final snapshot = results[2] as Map<String, dynamic>;
        _snapshot = snapshot.isEmpty ? null : snapshot;
        _loading = false;
        // 刷新后保留仍存在的详情；任务消失则关闭详情避免展示过期数据。
        if (!stillSelected) {
          _selectedTaskId = null;
          _evidence = [];
          _evidenceError = null;
          _evidenceLoading = false;
        }
      });
      if (_selectedTaskId != null) {
        await _loadEvidence(_selectedTaskId!);
      }
    } catch (error) {
      if (!mounted || seq != _requestSeq) return;
      setState(() {
        _error = '读取自动化任务失败：$error';
        _loading = false;
      });
    }
  }

  /// 拉取任务 Evidence 时间线；独立序号守卫 + 选中任务校验。
  Future<void> _loadEvidence(String taskId) async {
    final seq = ++_evidenceSeq;
    setState(() {
      _evidenceLoading = true;
      _evidenceError = null;
      _evidence = [];
    });
    try {
      final items = await _client.listEvidence(widget.project.id, taskId);
      if (!mounted || seq != _evidenceSeq || _selectedTaskId != taskId) return;
      setState(() {
        _evidence = items;
        _evidenceLoading = false;
      });
    } catch (error) {
      if (!mounted || seq != _evidenceSeq || _selectedTaskId != taskId) return;
      setState(() {
        _evidenceError = '读取 evidence 失败：$error';
        _evidenceLoading = false;
      });
    }
  }

  /// 点击任务行：展开详情并加载 Evidence。
  void _selectTask(String taskId) {
    setState(() {
      _selectedTaskId = taskId;
      _evidence = [];
      _evidenceError = null;
      _expandedEvidenceIds.clear();
    });
    _loadEvidence(taskId);
  }

  /// 收起详情。
  void _closeDetail() {
    setState(() {
      _selectedTaskId = null;
      _evidence = [];
      _evidenceError = null;
      _evidenceLoading = false;
    });
  }

  /// 打开创建任务对话框；[preferredAction] 来自泳道头「+ 任务」入口。
  Future<void> _openCreateDialog({String? preferredAction}) async {
    _title.clear();
    _goal.clear();
    _acceptance.clear();
    _prompt.clear();
    _createAction = preferredAction ?? 'backlog';
    await showDialog<void>(
      context: context,
      barrierDismissible: false,
      builder: (dialogContext) {
        return StatefulBuilder(
          builder: (dialogContext, setDialogState) {
            final theme = Theme.of(dialogContext);
            Future<void> onCompletePrompt() async {
              final prompt = _prompt.text.trim();
              if (prompt.isEmpty) {
                ScaffoldMessenger.of(context).showSnackBar(
                  const SnackBar(content: Text('请先输入简单 Prompt')),
                );
                return;
              }
              setDialogState(() => _completing = true);
              try {
                final completed = await _client.completePrompt(
                  widget.project.id,
                  prompt,
                  workingDirectory: widget.project.kind == 'local'
                      ? widget.project.path
                      : null,
                );
                _title.text = (completed['title'] as String? ?? '').trim();
                _goal.text = (completed['goal'] as String? ?? '').trim();
                _acceptance.text =
                    (completed['acceptanceCriteria'] as String? ?? '').trim();
              } catch (error) {
                if (mounted) {
                  ScaffoldMessenger.of(context).showSnackBar(
                    SnackBar(content: Text('AI 完善失败，请手动填写：$error')),
                  );
                }
              } finally {
                setDialogState(() => _completing = false);
              }
            }

            Future<void> onSubmit() async {
              final title = _title.text.trim();
              final goal = _goal.text.trim();
              final acceptance = _acceptance.text.trim();
              if (title.isEmpty || goal.isEmpty || acceptance.isEmpty) {
                ScaffoldMessenger.of(context).showSnackBar(
                  const SnackBar(content: Text('请填写任务标题、目标和验收标准')),
                );
                return;
              }
              final action = _createAction;
              setDialogState(() => _creating = true);
              var dialogClosed = false;
              try {
                await _client.createTask(
                  projectId: widget.project.id,
                  title: title,
                  goal: goal,
                  acceptanceCriteria: acceptance,
                  createAction: action,
                  clientRequestId: newClientOperationId(),
                );
                if (!mounted) return;
                dialogClosed = true;
                Navigator.of(context).pop();
                ScaffoldMessenger.of(context).showSnackBar(
                  SnackBar(
                    content: Text(_createActionSuccessLabels[action] ?? '任务已创建'),
                  ),
                );
                await _reload();
              } catch (error) {
                if (mounted) {
                  ScaffoldMessenger.of(context).showSnackBar(
                    SnackBar(content: Text('创建自动化任务失败：$error')),
                  );
                }
              } finally {
                if (dialogClosed) {
                  if (mounted) setState(() => _creating = false);
                } else {
                  setDialogState(() => _creating = false);
                }
              }
            }

            final busy = _creating || _completing;
            return PopScope(
              // busy 时禁止关闭（返回键 / 点遮罩 / 取消按钮全部无效）。
              canPop: !busy,
              child: Dialog(
                insetPadding: const EdgeInsets.symmetric(
                  horizontal: 40,
                  vertical: 24,
                ),
                child: ConstrainedBox(
                  constraints: const BoxConstraints(maxWidth: 420),
                  child: Padding(
                    padding: const EdgeInsets.all(16),
                    child: Column(
                      mainAxisSize: MainAxisSize.min,
                      crossAxisAlignment: CrossAxisAlignment.stretch,
                      children: [
                        Row(
                          children: [
                            Expanded(
                              child: Text(
                                '创建任务',
                                style: theme.textTheme.titleMedium,
                              ),
                            ),
                            IconButton(
                              tooltip: '关闭',
                              onPressed:
                                  busy
                                      ? null
                                      : () => Navigator.of(dialogContext).pop(),
                              icon: const Icon(Icons.close),
                            ),
                          ],
                        ),
                        Flexible(
                          child: SingleChildScrollView(
                            child: Column(
                              mainAxisSize: MainAxisSize.min,
                              crossAxisAlignment: CrossAxisAlignment.stretch,
                              children: [
                                TextField(
                                  key: const Key('create-prompt'),
                                  controller: _prompt,
                                  enabled: !busy,
                                  minLines: 1,
                                  maxLines: 3,
                                  decoration: const InputDecoration(
                                    labelText: '简单 Prompt',
                                    hintText: '一句话描述你想让 AI 完成的任务',
                                  ),
                                  onChanged: (_) => setDialogState(() {}),
                                ),
                                const SizedBox(height: 8),
                                OutlinedButton.icon(
                                  key: const Key('create-ai'),
                                  onPressed:
                                      busy || _prompt.text.trim().isEmpty
                                          ? null
                                          : onCompletePrompt,
                                  icon: _completing
                                      ? const SizedBox(
                                          width: 14,
                                          height: 14,
                                          child: CircularProgressIndicator(
                                            strokeWidth: 2,
                                          ),
                                        )
                                      : const Icon(Icons.auto_awesome,
                                          size: 16),
                                  label: Text(_completing ? '完善中…' : 'AI 完善'),
                                ),
                                const Divider(),
                                TextField(
                                  key: const Key('create-title'),
                                  controller: _title,
                                  enabled: !busy,
                                  decoration:
                                      const InputDecoration(labelText: '任务标题'),
                                ),
                                TextField(
                                  key: const Key('create-goal'),
                                  controller: _goal,
                                  enabled: !busy,
                                  minLines: 2,
                                  maxLines: 4,
                                  decoration:
                                      const InputDecoration(labelText: '目标'),
                                ),
                                TextField(
                                  key: const Key('create-acceptance'),
                                  controller: _acceptance,
                                  enabled: !busy,
                                  minLines: 2,
                                  maxLines: 4,
                                  decoration: const InputDecoration(
                                      labelText: '验收标准'),
                                ),
                                const SizedBox(height: 12),
                                SegmentedButton<String>(
                                  key: const Key('create-actions'),
                                  segments: const [
                                    ButtonSegment(
                                      value: 'backlog',
                                      label: Text('存入 Backlog'),
                                    ),
                                    ButtonSegment(
                                      value: 'todo',
                                      label: Text('存入 Todo'),
                                    ),
                                    ButtonSegment(
                                      value: 'start',
                                      label: Text('直接开始'),
                                    ),
                                  ],
                                  selected: {_createAction},
                                  onSelectionChanged: busy
                                      ? null
                                      : (selection) => setDialogState(
                                            () =>
                                                _createAction = selection.first,
                                          ),
                                ),
                              ],
                            ),
                          ),
                        ),
                        const SizedBox(height: 8),
                        Row(
                          mainAxisAlignment: MainAxisAlignment.end,
                          children: [
                            TextButton(
                              onPressed:
                                  busy
                                      ? null
                                      : () => Navigator.of(dialogContext).pop(),
                              child: const Text('取消'),
                            ),
                            const SizedBox(width: 8),
                            FilledButton(
                              key: const Key('create-submit'),
                              onPressed: busy ? null : onSubmit,
                              child: Text(_creating ? '创建中…' : '创建'),
                            ),
                          ],
                        ),
                      ],
                    ),
                  ),
                ),
              ),
            );
          },
        );
      },
    );
  }

  /// 重试 / 丢弃 outbox 条目；操作完成后刷新列表。
  Future<void> _runOutboxAction(
    String outboxId, {
    required bool discard,
  }) async {
    if (_outboxActionId != null) return;
    setState(() => _outboxActionId = outboxId);
    try {
      if (discard) {
        await _client.discardOutbox(widget.project.id, outboxId);
      } else {
        await _client.retryOutbox(widget.project.id, outboxId);
      }
      if (!mounted) return;
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text(discard ? '已丢弃该离线任务' : '已重新加入发送队列')),
      );
    } catch (error) {
      if (!mounted) return;
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(
          content: Text('${discard ? '丢弃失败' : '重试失败'}：$error'),
        ),
      );
    } finally {
      if (mounted) setState(() => _outboxActionId = null);
    }
    await _reload();
  }

  /// 丢弃前确认；确认后才真正调用 discard 路由。
  Future<void> _confirmDiscardOutbox(String outboxId) async {
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (dialogContext) => AlertDialog(
        title: const Text('丢弃离线任务'),
        content: const Text('确定丢弃这条失败的离线发送请求？'),
        actions: [
          TextButton(
            onPressed: () => Navigator.of(dialogContext).pop(false),
            child: const Text('取消'),
          ),
          FilledButton(
            key: const Key('outbox-discard-confirm'),
            onPressed: () => Navigator.of(dialogContext).pop(true),
            child: const Text('丢弃'),
          ),
        ],
      ),
    );
    if (confirmed != true) return;
    await _runOutboxAction(outboxId, discard: true);
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    if (_loading && _views.isEmpty) {
      return const Center(child: CircularProgressIndicator());
    }
    final split = automationSplitViews(_views);
    final groups = automationGroupByWorkflow(split.tasks);
    final laneIds = [
      ...kAutomationWorkflowStates,
      ...groups.keys
          .where((lane) => !kAutomationWorkflowStates.contains(lane)),
    ];
    return RefreshIndicator(
      onRefresh: _reload,
      child: ListView(
        key: const Key('automation-list'),
        physics: const AlwaysScrollableScrollPhysics(),
        padding: const EdgeInsets.all(12),
        children: [
          _buildHeader(theme),
          if (_error != null)
            Padding(
              padding: const EdgeInsets.only(top: 8),
              child: Text(
                _error!,
                style: TextStyle(color: theme.colorScheme.error),
              ),
            ),
          Padding(
            padding: const EdgeInsets.only(top: 12),
            child: FilledButton.icon(
              key: const Key('automation-create'),
              onPressed: () => _openCreateDialog(),
              icon: const Icon(Icons.add),
              label: const Text('创建任务'),
            ),
          ),
          for (final lane in laneIds)
            _buildLaneSection(
              theme,
              lane,
              groups[lane] ?? const <Map<String, dynamic>>[],
            ),
          if (split.tasks.isEmpty && split.pendingRemoteItems.isEmpty)
            const Padding(
              padding: EdgeInsets.symmetric(vertical: 24),
              child: Center(child: Text('当前项目暂无自动化任务')),
            ),
          if (split.pendingRemoteItems.isNotEmpty)
            _buildOutboxSection(theme, split.pendingRemoteItems),
          if (_experiments.isNotEmpty) ..._buildExperimentsSection(theme),
          const SizedBox(height: 24),
        ],
      ),
    );
  }

  /// 顶部：runtime 状态卡 + 手动刷新按钮。
  Widget _buildHeader(ThemeData theme) {
    return Card(
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 4),
        child: Row(
          children: [
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text('运行时状态', style: theme.textTheme.titleSmall),
                  Text(
                    _runtimeStatusLabel(_snapshot),
                    style: theme.textTheme.bodySmall,
                  ),
                ],
              ),
            ),
            IconButton(
              key: const Key('automation-refresh'),
              tooltip: '刷新',
              onPressed: _reload,
              icon: const Icon(Icons.refresh),
            ),
          ],
        ),
      ),
    );
  }

  /// 单个泳道：backlog/todo 恒显标题，其余泳道仅在有任务时渲染。
  Widget _buildLaneSection(
    ThemeData theme,
    String lane,
    List<Map<String, dynamic>> views,
  ) {
    final alwaysVisible = lane == 'backlog' || lane == 'todo';
    if (views.isEmpty && !alwaysVisible) return const SizedBox.shrink();
    return Padding(
      padding: const EdgeInsets.only(top: 16),
      child: Column(
        key: Key('automation-lane-$lane'),
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Text(
                _workflowLabels[lane] ?? lane,
                style: theme.textTheme.titleMedium,
              ),
              const SizedBox(width: 8),
              _badge(theme, '${views.length} 个任务'),
              const Spacer(),
              if (alwaysVisible)
                TextButton.icon(
                  key: Key('automation-lane-add-$lane'),
                  onPressed: () =>
                      _openCreateDialog(preferredAction: lane),
                  icon: const Icon(Icons.add, size: 16),
                  label: const Text('任务'),
                ),
            ],
          ),
          for (final view in views) _buildTaskCard(theme, view),
          if (views.isEmpty)
            Padding(
              padding: const EdgeInsets.symmetric(vertical: 8),
              child: Text('暂无任务', style: theme.textTheme.bodySmall),
            ),
        ],
      ),
    );
  }

  /// 任务行：标题 + 来源徽章 + workflow 徽章；点击展开详情卡。
  Widget _buildTaskCard(ThemeData theme, Map<String, dynamic> view) {
    final task = automationTaskOfView(view);
    if (task == null) return const SizedBox.shrink();
    final id = task['id'] as String? ?? '';
    final title = task['title'] as String? ?? id;
    final goal = task['goal'] as String? ?? '';
    final origin = view['origin'] as String? ?? 'local';
    final originLabel = origin == 'remote'
        ? '远端 ${view['deviceName'] as String? ?? 'unknown'}'
        : '本机';
    final workflow = task['workflowState'] as String? ?? 'backlog';
    final selected = _selectedTaskId == id;
    return Card(
      key: Key('automation-task-$id'),
      margin: const EdgeInsets.only(top: 8),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          ListTile(
            title: Text(title),
            subtitle: goal.isEmpty ? null : Text(goal, maxLines: 1),
            trailing: Wrap(
              spacing: 4,
              runSpacing: 4,
              alignment: WrapAlignment.end,
              children: [
                _badge(theme, originLabel, accent: true),
                _badge(theme, _workflowLabels[workflow] ?? workflow),
              ],
            ),
            onTap: () => selected ? _closeDetail() : _selectTask(id),
          ),
          if (selected) _buildDetailSection(theme, task),
        ],
      ),
    );
  }

  /// 任务详情：goal / 验收标准 / workflow / 运行阶段 / 阻塞原因 + Evidence 时间线。
  Widget _buildDetailSection(ThemeData theme, Map<String, dynamic> task) {
    final unknown = 'unknown';
    final attemptPhase = task['attemptPhase'] as String?;
    final blockedReason = task['blockedReason'] as String?;
    return Padding(
      key: const Key('automation-detail'),
      padding: const EdgeInsets.fromLTRB(16, 0, 16, 12),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          const Divider(),
          _kvRow(theme, '目标', task['goal'] as String? ?? unknown),
          _kvRow(theme, '验收标准', task['acceptanceCriteria'] as String? ?? unknown),
          _kvRow(
            theme,
            'Workflow',
            _workflowLabels[task['workflowState'] as String? ?? ''] ?? unknown,
          ),
          _kvRow(
            theme,
            '运行阶段',
            attemptPhase == null || attemptPhase.isEmpty ? unknown : attemptPhase,
          ),
          if (blockedReason != null && blockedReason.isNotEmpty)
            _kvRow(theme, '阻塞原因', blockedReason, danger: true),
          const SizedBox(height: 12),
          Row(
            children: [
              Text('Evidence', style: theme.textTheme.titleSmall),
              const SizedBox(width: 8),
              _badge(theme, '时间线 ${_evidence.length}'),
            ],
          ),
          if (_evidenceLoading)
            const Padding(
              padding: EdgeInsets.symmetric(vertical: 8),
              child: Row(
                children: [
                  SizedBox(
                    width: 14,
                    height: 14,
                    child: CircularProgressIndicator(strokeWidth: 2),
                  ),
                  SizedBox(width: 8),
                  Text('正在读取 evidence'),
                ],
              ),
            ),
          if (_evidenceError != null)
            Padding(
              padding: const EdgeInsets.symmetric(vertical: 8),
              child: Text(
                _evidenceError!,
                style: TextStyle(color: theme.colorScheme.error),
              ),
            ),
          if (!_evidenceLoading && _evidenceError == null && _evidence.isEmpty)
            const Padding(
              padding: EdgeInsets.symmetric(vertical: 8),
              child: Text('暂无 evidence'),
            ),
          for (final item in _evidence) _buildEvidenceItem(theme, item),
          const SizedBox(height: 8),
          Align(
            alignment: Alignment.centerRight,
            child: TextButton(onPressed: _closeDetail, child: const Text('关闭详情')),
          ),
        ],
      ),
    );
  }

  /// 单条 Evidence：kind 徽章 + 时间 + summary；content 可展开/收起。
  Widget _buildEvidenceItem(ThemeData theme, Map<String, dynamic> item) {
    final id = item['id'] as String? ?? '';
    final kind = item['kind'] as String? ?? 'generic';
    final content = item['content'] as String? ?? '';
    final expanded = _expandedEvidenceIds.contains(id);
    final summary = item['summary'] as String? ?? '';
    return Padding(
      key: Key('automation-evidence-$id'),
      padding: const EdgeInsets.symmetric(vertical: 6),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Expanded(
                child: Text(
                  item['title'] as String? ?? id,
                  style: theme.textTheme.bodyMedium
                      ?.copyWith(fontWeight: FontWeight.w600),
                ),
              ),
              _badge(theme, _evidenceKindLabels[kind] ?? _evidenceKindLabels['generic']!),
            ],
          ),
          Text(
            _formatAutomationTimestamp(item['createdAt'] as String? ?? ''),
            style: theme.textTheme.bodySmall,
          ),
          if (summary.isNotEmpty)
            Text(summary, style: theme.textTheme.bodySmall),
          if (expanded)
            Container(
              margin: const EdgeInsets.only(top: 4),
              padding: const EdgeInsets.all(8),
              width: double.infinity,
              color: theme.colorScheme.surfaceContainerHighest,
              child: Text(
                content,
                style: theme.textTheme.bodySmall
                    ?.copyWith(fontFamily: 'monospace'),
              ),
            )
          else if (content.isNotEmpty)
            TextButton(
              key: Key('automation-evidence-expand-$id'),
              onPressed: () =>
                  setState(() => _expandedEvidenceIds.add(id)),
              child: const Text('展开内容'),
            ),
          if (expanded && content.isNotEmpty)
            TextButton(
              onPressed: () =>
                  setState(() => _expandedEvidenceIds.remove(id)),
              child: const Text('收起内容'),
            ),
        ],
      ),
    );
  }

  /// 离线待同步区：标题/状态/目标设备/错误；failed 行提供 重试 与 丢弃（需确认）。
  Widget _buildOutboxSection(
    ThemeData theme,
    List<Map<String, dynamic>> items,
  ) {
    return Padding(
      padding: const EdgeInsets.only(top: 16),
      child: Column(
        key: const Key('automation-outbox'),
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Text('待发送到远端', style: theme.textTheme.titleMedium),
              const SizedBox(width: 8),
              _badge(theme, '${items.length} 个任务'),
            ],
          ),
          for (final item in items) _buildOutboxItem(theme, item),
        ],
      ),
    );
  }

  Widget _buildOutboxItem(ThemeData theme, Map<String, dynamic> item) {
    final id = item['id'] as String? ?? '';
    final status = item['status'] as String? ?? 'pending';
    final failed = status == 'failed';
    final busy = _outboxActionId != null;
    final lastError = item['lastError'] as String?;
    return Card(
      key: Key('automation-outbox-$id'),
      margin: const EdgeInsets.only(top: 8),
      child: Padding(
        padding: const EdgeInsets.all(12),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              children: [
                Expanded(
                  child: Text(
                    automationOutboxTitle(item),
                    style: theme.textTheme.titleSmall,
                  ),
                ),
                _badge(theme, _pendingStatusLabels[status] ?? status,
                    accent: failed),
              ],
            ),
            const SizedBox(height: 4),
            Text(
              '目标设备：${item['deviceName'] as String? ?? 'unknown'}',
              style: theme.textTheme.bodySmall,
            ),
            Text(
              lastError != null && lastError.isNotEmpty
                  ? '发送错误：$lastError'
                  : '远端路径：${item['remoteProjectPath'] as String? ?? ''}',
              style: theme.textTheme.bodySmall,
            ),
            if (failed)
              Padding(
                padding: const EdgeInsets.only(top: 8),
                child: Row(
                  children: [
                    OutlinedButton(
                      key: Key('automation-outbox-retry-$id'),
                      onPressed:
                          busy ? null : () => _runOutboxAction(id, discard: false),
                      child: const Text('重试'),
                    ),
                    const SizedBox(width: 8),
                    OutlinedButton(
                      key: Key('automation-outbox-discard-$id'),
                      onPressed: busy ? null : () => _confirmDiscardOutbox(id),
                      child: const Text('丢弃'),
                    ),
                  ],
                ),
              ),
          ],
        ),
      ),
    );
  }

  /// experiments 区：保持简单列表展示，固定在页面底部。
  List<Widget> _buildExperimentsSection(ThemeData theme) {
    return [
      Padding(
        padding: const EdgeInsets.only(top: 16),
        child: Text('experiments', style: theme.textTheme.titleMedium),
      ),
      for (final item in _experiments)
        ListTile(
          key: Key('automation-experiment-${item['id'] ?? ''}'),
          dense: true,
          title: Text(item['title'] as String? ?? item['id'] as String? ?? ''),
          subtitle: Text(item['state'] as String? ?? ''),
        ),
    ];
  }

  Widget _kvRow(
    ThemeData theme,
    String label,
    String value, {
    bool danger = false,
  }) {
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 2),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          SizedBox(
            width: 76,
            child: Text(label, style: theme.textTheme.bodySmall),
          ),
          Expanded(
            child: Text(
              value,
              style: theme.textTheme.bodyMedium?.copyWith(
                color: danger ? theme.colorScheme.error : null,
              ),
            ),
          ),
        ],
      ),
    );
  }

  Widget _badge(ThemeData theme, String text, {bool accent = false}) {
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 2),
      decoration: BoxDecoration(
        color: accent
            ? theme.colorScheme.primaryContainer
            : theme.colorScheme.surfaceContainerHighest,
        borderRadius: BorderRadius.circular(999),
      ),
      child: Text(
        text,
        style: theme.textTheme.labelSmall?.copyWith(
          color: accent
              ? theme.colorScheme.onPrimaryContainer
              : theme.colorScheme.onSurfaceVariant,
        ),
      ),
    );
  }
}
