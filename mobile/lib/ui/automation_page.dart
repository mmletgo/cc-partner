import 'dart:convert';

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

/// 运行态中文文案（对齐 web runState 标签；未知回落原值）。
const Map<String, String> _runStateLabels = {
  'idle': 'Idle',
  'queued': 'Queued',
  'preparing': 'Preparing',
  'running': 'Running',
  'verifying': 'Verifying',
  'retrying': 'Retrying',
  'blocked': 'Blocked',
  'delivering': 'Delivering',
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

/// 创建对话框内块成员草稿：一行标题/目标/验收三个输入。
///
/// Business Logic: 创建任务块需要逐条填写 2–8 个成员，成员行要能在对话框内增删。
/// Code Logic: 持有各自 TextEditingController；toBody 输出与 web create-block 相同的三字段。
class _BlockMemberDraft {
  final TextEditingController title = TextEditingController();
  final TextEditingController goal = TextEditingController();
  final TextEditingController acceptance = TextEditingController();

  /// 三字段是否都已填写（决定提交可用性）。
  bool get complete =>
      title.text.trim().isNotEmpty &&
      goal.text.trim().isNotEmpty &&
      acceptance.text.trim().isNotEmpty;

  /// 输出 create-block 成员请求体（只含 title/goal/acceptanceCriteria）。
  Map<String, String> toBody() => {
        'title': title.text.trim(),
        'goal': goal.text.trim(),
        'acceptanceCriteria': acceptance.text.trim(),
      };

  /// 释放行内控制器。
  void dispose() {
    title.dispose();
    goal.dispose();
    acceptance.dispose();
  }
}

/// 宽容读取 runtime 字符串；空白回退 fallback（对齐 web runtimeValue）。
String _runtimeValue(String? value, String fallback) {
  final trimmed = value?.trim();
  return (trimmed != null && trimmed.isNotEmpty) ? trimmed : fallback;
}

/// 宽容读取 runtime 数值；缺省为 0。
int _runtimeIntValue(dynamic value) => value is num ? value.toInt() : 0;

/// Evidence/缓存时间格式化：解析失败时原样返回，不让详情崩溃。
String _formatAutomationTimestamp(String value) {
  final date = DateTime.tryParse(value);
  if (date == null) return value;
  return '${date.toLocal()}'.split('.').first;
}

/// 当前时间的 ISO 字符串（UTC），用作 runtime 缓存接收时间。
String _nowIso() => DateTime.now().toUtc().toIso8601String();

class AutomationPage extends StatefulWidget {
  const AutomationPage({
    super.key,
    required this.book,
    required this.http,
    required this.project,
    this.focusTaskId,
    this.focusOutboxId,
    this.onFocusSession,
    this.onFocusMissing,
    this.onExternalMutation,
    @visibleForTesting AutomationClient? clientOverride,
  }) : _clientOverride = clientOverride;

  final AddressBook book;
  final LanHttpClient http;
  final ProjectSummary project;

  /// Attention 跳转聚焦：任务 id；加载完成后选中详情并高亮，找不到回调 [onFocusMissing]。
  final String? focusTaskId;

  /// Attention 跳转聚焦：outbox 条目 id；加载完成后高亮该行，找不到回调 [onFocusMissing]。
  final String? focusOutboxId;

  /// 「打开执行现场」：把任务绑定的 worktree/session 交给壳层切到终端面板；
  /// 单边 id 也回调（AND 改 OR 门控），壳层对缺失一边自行回落
  /// （对齐 web useMobileAutomationController 的 `worktreeId || sessionId`）。
  final void Function(String? worktreeId, String? sessionId)? onFocusSession;

  /// 聚焦 id 在列表中找不到时回调；壳层会回 Attention 并提示。
  final VoidCallback? onFocusMissing;

  /// experiments 审批、outbox 重试/丢弃等影响 Inbox 投影的变更后通知壳层。
  final VoidCallback? onExternalMutation;

  /// 仅测试注入：替换内部 Orchestrator HTTP 客户端。
  final AutomationClient? _clientOverride;

  @override
  State<AutomationPage> createState() => _AutomationPageState();
}

class _AutomationPageState extends State<AutomationPage> {
  late AutomationClient _client;
  List<Map<String, dynamic>> _views = [];
  List<Map<String, dynamic>> _experiments = [];
  String? _error;
  bool _loading = true;

  /// 主列表请求序号守卫：过期响应直接丢弃。
  int _requestSeq = 0;

  // ---- runtime 快照展示态（页面内缓存，带 projectId 归属防切项目串台）----
  /// 当前 live 缓存归属的项目 id；与当前项目不一致时缓存视为不存在。
  String? _runtimeProjectId;

  /// 本项目最后一次 remote live 快照（offline warm 显示用；local 不写入）。
  Map<String, dynamic>? _runtimeCache;

  /// 缓存接收时间（展示「最后更新」）。
  String? _runtimeCacheAt;

  /// 当前展示的快照（live/local 成功或 offline warm 缓存）。
  Map<String, dynamic>? _snapshot;

  /// 远端四态显示：live/offline/unsupported/unavailable；本机成功为 null。
  String? _runtimeStatus;

  /// 展示用缓存时间（仅 warm offline 与 live 成功时有值）。
  String? _runtimeDisplayCachedAt;

  /// runtime 请求失败信息（展示在快照条内，不影响任务动作）。
  String? _runtimeError;

  bool _runtimeLoading = false;

  /// runtime 快照请求序号守卫。
  int _runtimeSeq = 0;

  String? _selectedTaskId;
  List<Map<String, dynamic>> _evidence = [];
  bool _evidenceLoading = false;
  String? _evidenceError;
  final Set<String> _expandedEvidenceIds = {};

  /// Evidence 请求序号守卫：切换任务后过期响应不覆盖当前详情。
  int _evidenceSeq = 0;

  String? _outboxActionId;
  String? _experimentActionId;

  /// 块组展开状态（blockId 集合）。
  final Set<String> _expandedBlockIds = {};

  /// Attention 聚焦命中的 outbox 条目 id（高亮展示）。
  String? _focusedOutboxId;

  /// 已应用过的聚焦 id，避免重复回调 onFocusMissing / 重复选中。
  String? _appliedFocusTaskId;
  String? _appliedFocusOutboxId;

  /// 任务块创建能力（remote 项目需 owner peer 支持 task-blocks 能力）。
  bool _canCreateTaskBlock = false;

  final _title = TextEditingController();
  final _goal = TextEditingController();
  final _acceptance = TextEditingController();
  final _prompt = TextEditingController();
  final _blockTitle = TextEditingController();

  /// 创建对话框块成员草稿（2..N 行）。
  List<_BlockMemberDraft> _blockMembers = [];

  /// 是否块创建模式（append 模式由 [_appendBlockId] 表达）。
  bool _blockMode = false;

  /// 非 null 时对话框为块末尾追加模式，值为目标块 id。
  String? _appendBlockId;

  String _createAction = 'backlog';
  bool _creating = false;
  bool _completing = false;

  /// 一次逻辑创建提交的幂等键：表单内容不变的重试复用；成功/关闭/表单变更后清空。
  String? _createClientRequestId;
  String? _createClientRequestFingerprint;

  @override
  void initState() {
    super.initState();
    _client = widget._clientOverride ??
        AutomationClient(widget.http, widget.book.active!.baseUrl);
    _initPeerCapability();
    _reload();
  }

  @override
  void didUpdateWidget(covariant AutomationPage oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.project.id != widget.project.id) {
      // 项目切换：重建客户端（base url 可能随 active 设备变化），
      // 清空全部项目态并重拉，防止旧项目快照/列表串台。
      _client = widget._clientOverride ??
          AutomationClient(widget.http, widget.book.active!.baseUrl);
      _resetForProjectChange();
      _initPeerCapability();
      _reload();
    } else if (oldWidget.focusTaskId != widget.focusTaskId ||
        oldWidget.focusOutboxId != widget.focusOutboxId) {
      _maybeApplyFocus();
    }
  }

  @override
  void dispose() {
    _title.dispose();
    _goal.dispose();
    _acceptance.dispose();
    _prompt.dispose();
    _blockTitle.dispose();
    for (final member in _blockMembers) {
      member.dispose();
    }
    super.dispose();
  }

  /// 初始化任务块创建能力（对齐 web canCreateOrchestratorTaskBlock）。
  ///
  /// Business Logic: remote shortcut 的「+块」必须看 owner 设备协议能力；本机项目始终可建块。
  /// Code Logic: local/非 remote 直接置真；remote 异步查项目 deviceId + 设备清单，
  /// 任何失败都 fail-closed 保持 false。
  void _initPeerCapability() {
    final kind = widget.project.kind;
    if (kind == null) {
      _canCreateTaskBlock = false;
      return;
    }
    if (kind != 'remote') {
      _canCreateTaskBlock = true;
      return;
    }
    _canCreateTaskBlock = false;
    _loadRemotePeerCapability();
  }

  /// remote 项目：projects/list 取 deviceId，再匹配 /api/mobile/devices 的能力清单。
  Future<void> _loadRemotePeerCapability() async {
    final projectId = widget.project.id;
    String? deviceId;
    List<Map<String, dynamic>> devices = const [];
    try {
      deviceId = await _client.projectOwnerDeviceId(projectId);
      devices = await _client.listDevices();
    } catch (_) {
      // 拿不到设备信息时按不支持处理（fail-closed）。
    }
    if (!mounted || widget.project.id != projectId) return;
    Map<String, dynamic>? owner;
    for (final device in devices) {
      if (deviceId != null && device['id'] == deviceId) {
        owner = device;
        break;
      }
    }
    setState(() {
      _canCreateTaskBlock = automationPeerSupportsTaskBlocks(owner);
    });
  }

  /// 项目切换时清空页面态：列表/实验组/runtime 缓存/选中/聚焦/对话框草稿。
  void _resetForProjectChange() {
    _requestSeq++;
    _runtimeSeq++;
    _evidenceSeq++;
    _views = [];
    _experiments = [];
    _error = null;
    _loading = true;
    _runtimeProjectId = null;
    _runtimeCache = null;
    _runtimeCacheAt = null;
    _snapshot = null;
    _runtimeStatus = null;
    _runtimeDisplayCachedAt = null;
    _runtimeError = null;
    _runtimeLoading = false;
    _selectedTaskId = null;
    _evidence = [];
    _evidenceError = null;
    _evidenceLoading = false;
    _expandedEvidenceIds.clear();
    _focusedOutboxId = null;
    _appliedFocusTaskId = null;
    _appliedFocusOutboxId = null;
    _outboxActionId = null;
    _experimentActionId = null;
    _expandedBlockIds.clear();
    _canCreateTaskBlock = false;
    _createClientRequestId = null;
    _createClientRequestFingerprint = null;
    for (final member in _blockMembers) {
      member.dispose();
    }
    _blockMembers = [];
  }

  /// 拉取泳道视图 / 实验组；runtime 快照独立请求；请求序号过期则丢弃整个响应。
  Future<void> _reload() async {
    final seq = ++_requestSeq;
    setState(() {
      _loading = true;
      _error = null;
    });
    _loadRuntimeSnapshot();
    try {
      final results = await Future.wait<dynamic>([
        _client.listViews(widget.project.id),
        _client
            .listExperiments(widget.project.id)
            .catchError((_) => <Map<String, dynamic>>[]),
      ]);
      if (!mounted || seq != _requestSeq) return;
      final views = results[0] as List<Map<String, dynamic>>;
      final stillSelected = automationSplitViews(views).tasks.any(
            (view) => automationTaskOfView(view)?['id'] == _selectedTaskId,
          );
      setState(() {
        _views = views;
        _experiments = results[1] as List<Map<String, dynamic>>;
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
      _maybeApplyFocus();
    } catch (error) {
      if (!mounted || seq != _requestSeq) return;
      setState(() {
        _error = '读取自动化任务失败：$error';
        _loading = false;
      });
      _maybeApplyFocus();
    }
  }

  /// 拉取 runtime 快照并归并展示态（对齐 web mobileRuntimeSnapshotStore 语义）。
  ///
  /// Business Logic: 面板需要展示 remote-aware 四态；offline warm 时保留本项目上一次
  /// remote live 快照供显示，缓存不驱动任务动作，切项目后缓存不得串台。
  /// Code Logic: 请求前用同项目 live 缓存做骨架；成功按 remoteStatus 归并
  /// （live 写缓存 / local 不写 / offline 复用缓存 / unsupported+unavailable 清缓存）；
  /// 失败仅网络类错误 + 有缓存才标 offline warm。
  Future<void> _loadRuntimeSnapshot() async {
    final projectId = widget.project.id;
    final seq = ++_runtimeSeq;
    final cached = _runtimeProjectId == projectId ? _runtimeCache : null;
    final cachedAt = _runtimeProjectId == projectId ? _runtimeCacheAt : null;
    setState(() {
      _runtimeLoading = true;
      _runtimeError = null;
      _snapshot = cached;
      _runtimeStatus = cached != null ? 'live' : null;
      _runtimeDisplayCachedAt = cached != null ? cachedAt : null;
    });
    Map<String, dynamic>? snapshot;
    Object? failure;
    try {
      snapshot = await _client.runtimeSnapshot(projectId);
    } catch (error) {
      failure = error;
    }
    if (!mounted || seq != _runtimeSeq) return;
    if (failure != null) {
      // 仅网络类失败（非后端 HTTP 错误信封）+ live 缓存才算 offline warm。
      final networkLike = failure is! LanHttpException;
      final warm = cached != null && networkLike;
      setState(() {
        _runtimeLoading = false;
        _runtimeError = '读取运行时状态失败：$failure';
        _snapshot = warm ? cached : null;
        _runtimeStatus = warm
            ? 'offline'
            : (cached != null ? 'unavailable' : null);
        _runtimeDisplayCachedAt = warm ? cachedAt : null;
      });
      return;
    }
    final remoteProjectId = snapshot!['projectId'];
    if (remoteProjectId is String && remoteProjectId.isNotEmpty && remoteProjectId != projectId) {
      // 快照不属于当前项目：按空态展示，防止串台。
      setState(() {
        _runtimeLoading = false;
        _snapshot = null;
        _runtimeStatus = null;
        _runtimeDisplayCachedAt = null;
      });
      return;
    }
    final status = snapshot['remoteStatus'] as String?;
    switch (status) {
      case 'live':
        final receivedAt = _nowIso();
        setState(() {
          _runtimeLoading = false;
          _runtimeProjectId = projectId;
          _runtimeCache = snapshot;
          _runtimeCacheAt = receivedAt;
          _snapshot = snapshot;
          _runtimeStatus = 'live';
          _runtimeDisplayCachedAt = receivedAt;
          _runtimeError = null;
        });
      case 'local':
        // 本机成功不写入 remote live 缓存；展示层状态归一为 null（显示「本机」）。
        setState(() {
          _runtimeLoading = false;
          _runtimeProjectId = projectId;
          _snapshot = snapshot;
          _runtimeStatus = null;
          _runtimeDisplayCachedAt = null;
          _runtimeError = null;
        });
      case 'offline':
        final warm = _runtimeProjectId == projectId ? _runtimeCache : null;
        setState(() {
          _runtimeLoading = false;
          _snapshot = warm;
          _runtimeStatus = 'offline';
          _runtimeDisplayCachedAt = warm != null ? _runtimeCacheAt : null;
          _runtimeError = null;
        });
      default:
        // unsupported / unavailable / 未知：清空本项目缓存，避免跨状态误用。
        setState(() {
          _runtimeLoading = false;
          _runtimeProjectId = projectId;
          _runtimeCache = null;
          _runtimeCacheAt = null;
          _snapshot = null;
          _runtimeStatus =
              (status != null && status.isNotEmpty) ? status : null;
          _runtimeDisplayCachedAt = null;
          _runtimeError = null;
        });
    }
  }

  /// runtime 徽章文案：本机归一显示「本机」；cold offline / 未知为中性「状态未知」。
  String _runtimeBadgeLabel() {
    if (_runtimeLoading && _snapshot == null) return '刷新中';
    if (_snapshot?['remoteStatus'] == 'local') return '本机';
    switch (_runtimeStatus) {
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

  /// 是否展示「缓存仅用于显示」提示：仅 warm offline（有快照 + 有缓存时间）。
  bool get _showRuntimeCachedHint =>
      _runtimeStatus == 'offline' && _snapshot != null && _runtimeDisplayCachedAt != null;

  /// Attention 聚焦应用：列表加载完成后定位 focusTaskId / focusOutboxId。
  ///
  /// Business Logic: Attention 跳转到自动化后应直接看到目标条目；条目被清理时
  /// 需要通知壳层回退并提示，而不是静默无反应。
  /// Code Logic: 每个聚焦 id 只应用一次（applied ref 守卫）；task 命中选中详情，
  /// outbox 命中高亮该行；任一未命中回调 onFocusMissing。
  void _maybeApplyFocus() {
    if (_loading) return;
    final split = automationSplitViews(_views);

    final focusTaskId = widget.focusTaskId;
    if (focusTaskId != null &&
        focusTaskId.isNotEmpty &&
        _appliedFocusTaskId != focusTaskId) {
      _appliedFocusTaskId = focusTaskId;
      Map<String, dynamic>? match;
      for (final view in split.tasks) {
        if (automationTaskOfView(view)?['id'] == focusTaskId) {
          match = view;
          break;
        }
      }
      if (match != null) {
        _selectTask(focusTaskId);
      } else {
        widget.onFocusMissing?.call();
      }
    }

    final focusOutboxId = widget.focusOutboxId;
    if (focusOutboxId != null &&
        focusOutboxId.isNotEmpty &&
        _appliedFocusOutboxId != focusOutboxId) {
      _appliedFocusOutboxId = focusOutboxId;
      final found = split.pendingRemoteItems.any(
        (item) => item['id'] == focusOutboxId,
      );
      setState(() {
        _focusedOutboxId = found ? focusOutboxId : null;
      });
      if (!found) {
        widget.onFocusMissing?.call();
      }
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

  /// 块成员上移/下移：只交换相邻成员后提交完整置换。
  void _moveBlockMember(AutomationBoardItem item, int index, int delta) {
    final ids = item.members.map((member) => member.id).toList();
    final target = index + delta;
    if (target < 0 || target >= ids.length) return;
    final currentId = ids[index];
    final targetId = ids[target];
    if (currentId.isEmpty || targetId.isEmpty) return;
    ids[index] = targetId;
    ids[target] = currentId;
    _reorderBlock(item.blockId!, ids);
  }

  /// 提交块成员重排；按返回的最新成员视图 upsert 进当前列表。
  Future<void> _reorderBlock(String blockId, List<String> orderedTaskIds) async {
    try {
      final updated = await _client.reorderBlockMembers(
        projectId: widget.project.id,
        blockId: blockId,
        orderedTaskIds: orderedTaskIds,
        clientRequestId: newClientOperationId(),
      );
      if (!mounted) return;
      setState(() {
        for (final view in updated) {
          _views = automationUpsertView(_views, view);
        }
      });
    } catch (error) {
      if (!mounted) return;
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text('任务块重排失败：$error')),
      );
    }
  }

  /// 重置创建/追加对话框草稿与幂等键；打开新对话框视为新逻辑提交周期。
  void _resetCreateForm() {
    _title.clear();
    _goal.clear();
    _acceptance.clear();
    _prompt.clear();
    _blockTitle.clear();
    for (final member in _blockMembers) {
      member.dispose();
    }
    _blockMembers = [_newBlockMemberDraft(), _newBlockMemberDraft()];
    _creating = false;
    _completing = false;
    _createClientRequestId = null;
    _createClientRequestFingerprint = null;
  }

  /// 新建一个空白块成员草稿。
  _BlockMemberDraft _newBlockMemberDraft() => _BlockMemberDraft();

  /// 打开创建任务对话框；[preferredAction] 来自泳道头「+ 任务 / + 块」入口。
  Future<void> _openCreateDialog({
    String? preferredAction,
    bool blockMode = false,
  }) async {
    _resetCreateForm();
    _createAction = preferredAction ?? 'backlog';
    _blockMode = blockMode && _canCreateTaskBlock;
    _appendBlockId = null;
    await _showCreateDialog();
  }

  /// 打开块末尾追加对话框：只填 title/goal/acceptance 三字段。
  Future<void> _openAppendDialog(String blockId) async {
    _resetCreateForm();
    _appendBlockId = blockId;
    _blockMode = false;
    _createAction = 'backlog';
    await _showCreateDialog();
  }

  /// 表单指纹 → 幂等键：内容不变的重试复用同一 clientRequestId。
  String _mintClientRequestId(String fingerprint) {
    if (_createClientRequestId == null ||
        _createClientRequestFingerprint != fingerprint) {
      _createClientRequestId = newClientOperationId();
      _createClientRequestFingerprint = fingerprint;
    }
    return _createClientRequestId!;
  }

  /// 渲染创建/追加共享对话框（StatefulBuilder 管理局部 busy 重建）。
  Future<void> _showCreateDialog() async {
    await showDialog<void>(
      context: context,
      barrierDismissible: false,
      builder: (dialogContext) {
        return StatefulBuilder(
          builder: (dialogContext, setDialogState) {
            final theme = Theme.of(dialogContext);
            final isAppend = _appendBlockId != null;
            final busy = _creating || _completing;

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

            /// 单任务创建：Backlog/Todo/Start 三动作 + 指纹幂等键。
            Future<void> onSubmitTask(String action) async {
              final title = _title.text.trim();
              final goal = _goal.text.trim();
              final acceptance = _acceptance.text.trim();
              if (title.isEmpty || goal.isEmpty || acceptance.isEmpty) {
                ScaffoldMessenger.of(context).showSnackBar(
                  const SnackBar(content: Text('请填写任务标题、目标和验收标准')),
                );
                return;
              }
              final fingerprint = [
                widget.project.id,
                title,
                goal,
                acceptance,
                action,
              ].join('\u0001');
              final clientRequestId = _mintClientRequestId(fingerprint);
              setDialogState(() => _creating = true);
              var dialogClosed = false;
              try {
                final created = await _client.createTask(
                  projectId: widget.project.id,
                  title: title,
                  goal: goal,
                  acceptanceCriteria: acceptance,
                  createAction: action,
                  clientRequestId: clientRequestId,
                );
                if (!mounted) return;
                setState(() {
                  _views = automationUpsertView(_views, created);
                  final createdTask = automationTaskOfView(created);
                  final createdId = createdTask?['id'] as String?;
                  if (createdTask != null && createdId != null && createdId.isNotEmpty) {
                    _selectedTaskId = createdId;
                    _evidence = [];
                    _evidenceError = null;
                    _expandedEvidenceIds.clear();
                  }
                });
                dialogClosed = true;
                Navigator.of(context).pop();
                ScaffoldMessenger.of(context).showSnackBar(
                  SnackBar(
                    content: Text(_createActionSuccessLabels[action] ?? '任务已创建'),
                  ),
                );
                // 成功即结束本逻辑提交周期，清空幂等键。
                _createClientRequestId = null;
                _createClientRequestFingerprint = null;
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

            /// 任务块创建：块标题 + 2..8 个完整成员，一次 create-block 提交。
            Future<void> onSubmitBlock(String action) async {
              if (!_canCreateTaskBlock) {
                ScaffoldMessenger.of(context).showSnackBar(
                  const SnackBar(content: Text('对端不支持任务块。')),
                );
                return;
              }
              final blockTitle = _blockTitle.text.trim();
              final members =
                  _blockMembers.map((member) => member.toBody()).toList();
              if (blockTitle.isEmpty ||
                  members.length < kAutomationBlockMinMembers ||
                  members.length > kAutomationBlockMaxMembers ||
                  members.any((member) =>
                      member['title']!.isEmpty ||
                      member['goal']!.isEmpty ||
                      member['acceptanceCriteria']!.isEmpty)) {
                ScaffoldMessenger.of(context).showSnackBar(
                  const SnackBar(content: Text('任务块需要标题，且包含 2–8 个完整成员')),
                );
                return;
              }
              final fingerprint = [
                widget.project.id,
                blockTitle,
                jsonEncode(members),
                action,
              ].join('\u0001');
              final clientRequestId = _mintClientRequestId(fingerprint);
              setDialogState(() => _creating = true);
              var dialogClosed = false;
              try {
                final created = await _client.createBlock(
                  projectId: widget.project.id,
                  title: blockTitle,
                  members: members,
                  createAction: action,
                  clientRequestId: clientRequestId,
                );
                if (!mounted) return;
                setState(() {
                  _views = automationUpsertBlockCreated(_views, created);
                });
                dialogClosed = true;
                Navigator.of(context).pop();
                ScaffoldMessenger.of(context).showSnackBar(
                  SnackBar(
                    content: Text(_createActionSuccessLabels[action] ?? '任务已创建'),
                  ),
                );
                _blockTitle.clear();
                for (final member in _blockMembers) {
                  member.dispose();
                }
                _blockMembers = [_newBlockMemberDraft(), _newBlockMemberDraft()];
                _prompt.clear();
                _createClientRequestId = null;
                _createClientRequestFingerprint = null;
                await _reload();
              } catch (error) {
                if (mounted) {
                  ScaffoldMessenger.of(context).showSnackBar(
                    SnackBar(content: Text('任务块创建失败：$error')),
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

            /// 块末尾追加：只提交三字段，成功后 upsert 新成员视图。
            Future<void> onSubmitAppend() async {
              final title = _title.text.trim();
              final goal = _goal.text.trim();
              final acceptance = _acceptance.text.trim();
              if (title.isEmpty || goal.isEmpty || acceptance.isEmpty) {
                ScaffoldMessenger.of(context).showSnackBar(
                  const SnackBar(content: Text('请填写任务标题、目标和验收标准')),
                );
                return;
              }
              setDialogState(() => _creating = true);
              var dialogClosed = false;
              try {
                final created = await _client.appendBlockMember(
                  projectId: widget.project.id,
                  blockId: _appendBlockId!,
                  title: title,
                  goal: goal,
                  acceptanceCriteria: acceptance,
                  clientRequestId: newClientOperationId(),
                );
                if (!mounted) return;
                setState(() {
                  _views = automationUpsertView(_views, created);
                });
                dialogClosed = true;
                Navigator.of(context).pop();
                _appendBlockId = null;
                await _reload();
              } catch (error) {
                if (mounted) {
                  ScaffoldMessenger.of(context).showSnackBar(
                    SnackBar(content: Text('追加任务失败：$error')),
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

            final submitDisabled = busy ||
                (isAppend
                    ? false
                    : _blockMode &&
                        !_canCreateTaskBlock);

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
                                isAppend ? '在末尾添加任务' : '创建任务',
                                style: theme.textTheme.titleMedium,
                              ),
                            ),
                            IconButton(
                              tooltip: '关闭',
                              onPressed:
                                  busy
                                      ? null
                                      : () {
                                          _appendBlockId = null;
                                          _createClientRequestId = null;
                                          _createClientRequestFingerprint = null;
                                          Navigator.of(dialogContext).pop();
                                        },
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
                                if (!isAppend) ...[
                                  SegmentedButton<String>(
                                    key: const Key('create-mode'),
                                    segments: const [
                                      ButtonSegment(
                                        value: 'task',
                                        label: Text('任务'),
                                      ),
                                      ButtonSegment(
                                        value: 'block',
                                        label: Text('任务块'),
                                      ),
                                    ],
                                    selected: {_blockMode ? 'block' : 'task'},
                                    onSelectionChanged: busy
                                        ? null
                                        : (selection) {
                                            final mode = selection.first;
                                            // 能力不支持时忽略块模式切换（fail-closed）。
                                            if (mode == 'block' &&
                                                !_canCreateTaskBlock) {
                                              return;
                                            }
                                            setDialogState(() {
                                              _blockMode = mode == 'block';
                                            });
                                          },
                                  ),
                                  if (!_canCreateTaskBlock)
                                    Padding(
                                      padding: const EdgeInsets.only(top: 6),
                                      child: Text(
                                        '对端不支持任务块。',
                                        key: const Key(
                                          'create-block-unsupported',
                                        ),
                                        style: theme.textTheme.bodySmall,
                                      ),
                                    ),
                                  const SizedBox(height: 8),
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
                                ],
                                if (isAppend || !_blockMode) ...[
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
                                ] else ...[
                                  TextField(
                                    key: const Key('block-title'),
                                    controller: _blockTitle,
                                    enabled: !busy,
                                    decoration: const InputDecoration(
                                        labelText: '块标题'),
                                  ),
                                  for (var i = 0; i < _blockMembers.length; i++)
                                    _buildBlockMemberFields(
                                      theme,
                                      setDialogState,
                                      index: i,
                                      busy: busy,
                                    ),
                                  if (_blockMembers.length <
                                      kAutomationBlockMaxMembers)
                                    OutlinedButton.icon(
                                      key: const Key('block-add-member'),
                                      onPressed: busy
                                          ? null
                                          : () => setDialogState(() {
                                                _blockMembers
                                                    .add(_newBlockMemberDraft());
                                              }),
                                      icon: const Icon(Icons.add, size: 16),
                                      label: const Text('添加成员'),
                                    ),
                                ],
                                const SizedBox(height: 12),
                                if (!isAppend) ...[
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
                              ],
                            ),
                          ),
                        ),
                        const SizedBox(height: 8),
                        Row(
                          mainAxisAlignment: MainAxisAlignment.end,
                          children: [
                            if (!isAppend) ...[
                              TextButton(
                                onPressed:
                                    busy
                                        ? null
                                        : () {
                                            _appendBlockId = null;
                                            _createClientRequestId = null;
                                            _createClientRequestFingerprint = null;
                                            Navigator.of(dialogContext).pop();
                                          },
                                child: const Text('取消'),
                              ),
                              const SizedBox(width: 8),
                            ],
                            FilledButton(
                              key: Key(isAppend ? 'append-submit' : 'create-submit'),
                              onPressed:
                                  submitDisabled
                                      ? null
                                      : () {
                                          final action = _createAction;
                                          if (isAppend) {
                                            onSubmitAppend();
                                          } else if (_blockMode) {
                                            onSubmitBlock(action);
                                          } else {
                                            onSubmitTask(action);
                                          }
                                        },
                              child: Text(
                                isAppend
                                    ? (_creating ? '创建中…' : '追加任务')
                                    : (_creating ? '创建中…' : '创建'),
                              ),
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

  /// 块成员行：序号 + 移除按钮 + 标题/目标/验收三个输入。
  Widget _buildBlockMemberFields(
    ThemeData theme,
    void Function(VoidCallback) setDialogState, {
    required int index,
    required bool busy,
  }) {
    final member = _blockMembers[index];
    return Padding(
      padding: const EdgeInsets.only(top: 8),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Text('第 ${index + 1} 步', style: theme.textTheme.labelLarge),
              const Spacer(),
              if (_blockMembers.length > kAutomationBlockMinMembers)
                TextButton(
                  key: Key('block-member-remove-$index'),
                  onPressed:
                      busy
                          ? null
                          : () => setDialogState(() {
                                _blockMembers[index].dispose();
                                _blockMembers.removeAt(index);
                              }),
                  child: const Text('移除成员'),
                ),
            ],
          ),
          TextField(
            key: Key('block-member-title-$index'),
            controller: member.title,
            enabled: !busy,
            decoration: const InputDecoration(labelText: '任务标题'),
          ),
          TextField(
            key: Key('block-member-goal-$index'),
            controller: member.goal,
            enabled: !busy,
            minLines: 2,
            maxLines: 3,
            decoration: const InputDecoration(labelText: '目标'),
          ),
          TextField(
            key: Key('block-member-acceptance-$index'),
            controller: member.acceptance,
            enabled: !busy,
            minLines: 2,
            maxLines: 3,
            decoration: const InputDecoration(labelText: '验收标准'),
          ),
        ],
      ),
    );
  }

  /// 重试 / 丢弃 outbox 条目；操作完成后刷新列表并通知壳层失效 Inbox 投影。
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
      widget.onExternalMutation?.call();
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

  /// 采纳实验组推荐 winner / 取消实验组；成功后刷新列表并通知壳层失效 Inbox 投影。
  Future<void> _runExperimentAction(
    String experimentId,
    Future<Map<String, dynamic>> Function() action,
    String successMessage,
  ) async {
    if (_experimentActionId != null) return;
    setState(() => _experimentActionId = experimentId);
    try {
      final updated = await action();
      if (!mounted) return;
      setState(() {
        _experiments = [
          for (final item in _experiments)
            item['id'] == updated['id'] ? updated : item,
        ];
      });
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text(successMessage)),
      );
      await _reload();
      widget.onExternalMutation?.call();
    } catch (error) {
      if (!mounted) return;
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text('操作失败：$error')),
      );
    } finally {
      if (mounted) setState(() => _experimentActionId = null);
    }
  }

  /// 采纳推荐：取推荐 winner id 调 approve-winner。
  Future<void> _approveExperiment(Map<String, dynamic> experiment) async {
    final experimentId = experiment['id'] as String? ?? '';
    final winnerTaskId = automationExperimentRecommendedTaskId(experiment);
    if (experimentId.isEmpty || winnerTaskId == null) return;
    await _runExperimentAction(
      experimentId,
      () => _client.approveExperimentWinner(experimentId, winnerTaskId),
      '已采纳推荐结果',
    );
  }

  /// 取消整组实验。
  Future<void> _cancelExperiment(Map<String, dynamic> experiment) async {
    final experimentId = experiment['id'] as String? ?? '';
    if (experimentId.isEmpty) return;
    await _runExperimentAction(
      experimentId,
      () => _client.cancelExperiment(experimentId),
      '已取消实验组',
    );
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    if (_loading && _views.isEmpty) {
      return const Center(child: CircularProgressIndicator());
    }
    final split = automationSplitViews(_views);
    final groups = automationGroupBoardItems(split.tasks);
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
          _buildRuntimeCard(theme),
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
              groups[lane] ?? const <AutomationBoardItem>[],
            ),
          if (split.tasks.isEmpty && split.pendingRemoteItems.isEmpty)
            const Padding(
              padding: EdgeInsets.symmetric(vertical: 24),
              child: Center(child: Text('当前项目暂无自动化任务')),
            ),
          if (split.pendingRemoteItems.isNotEmpty)
            _buildOutboxSection(theme, split.pendingRemoteItems),
          _buildExperimentsSection(theme),
          const SizedBox(height: 24),
        ],
      ),
    );
  }

  /// 顶部：runtime 状态卡（四态徽章 + 快照摘要 + 缓存提示）+ 手动刷新按钮。
  Widget _buildRuntimeCard(ThemeData theme) {
    final snapshot = _snapshot;
    final generatedAt = snapshot?['generatedAt'] as String? ?? '';
    final latestTickRaw = snapshot?['latestTickAt'] as String?;
    final latestTickText =
        (latestTickRaw == null || latestTickRaw.trim().isEmpty)
            ? '未知'
            : _formatAutomationTimestamp(latestTickRaw);
    final latestError = snapshot?['latestError'] as String? ?? '';
    final running = asObjectList(snapshot?['runningTasks']);
    final retrying = asObjectList(snapshot?['retryingTasks']);
    final recentEvents = asObjectList(snapshot?['recentEvents']);
    return Card(
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              children: [
                Expanded(
                  child: Text('运行时状态', style: theme.textTheme.titleSmall),
                ),
                KeyedSubtree(
                  key: const Key('automation-runtime-badge'),
                  child: _badge(theme, _runtimeBadgeLabel(), accent: true),
                ),
                IconButton(
                  key: const Key('automation-refresh'),
                  tooltip: '刷新',
                  onPressed: _reload,
                  icon: const Icon(Icons.refresh),
                ),
              ],
            ),
            if (_runtimeError != null)
              Text(
                _runtimeError!,
                style: TextStyle(color: theme.colorScheme.error),
              ),
            if (snapshot == null && _runtimeLoading)
              const Padding(
                padding: EdgeInsets.symmetric(vertical: 4),
                child: Text('刷新中'),
              ),
            if (snapshot != null) ...[
              if (generatedAt.isNotEmpty)
                Text(
                  '生成时间 ${_formatAutomationTimestamp(generatedAt)}',
                  style: theme.textTheme.bodySmall,
                ),
              Text(
                '最近 tick $latestTickText',
                style: theme.textTheme.bodySmall,
              ),
              Text(
                '槽位 ${_runtimeIntValue(snapshot['slotsUsed'])}/${_runtimeIntValue(snapshot['maxConcurrentTasks'])}'
                ' · 运行 ${running.length} · 重试 ${retrying.length}',
                style: theme.textTheme.bodySmall,
              ),
              if (latestError.isNotEmpty)
                Text(
                  '最近错误：$latestError',
                  style: theme.textTheme.bodySmall
                      ?.copyWith(color: theme.colorScheme.error),
                ),
              if (recentEvents.isNotEmpty) ...[
                Padding(
                  padding: const EdgeInsets.only(top: 4),
                  child: Text('最近事件', style: theme.textTheme.labelLarge),
                ),
                for (final event in recentEvents)
                  Text(
                    '${_runtimeValue(event['taskTitle'] as String?, event['id'] as String? ?? '')}: '
                    '${_runtimeValue(event['message'] as String?, '')}',
                    key: Key('automation-runtime-event-${event['id'] ?? ''}'),
                    style: theme.textTheme.bodySmall,
                  ),
              ],
            ],
            if (_runtimeDisplayCachedAt != null)
              Text(
                '最后更新 ${_formatAutomationTimestamp(_runtimeDisplayCachedAt!)}',
                style: theme.textTheme.bodySmall,
              ),
            if (_showRuntimeCachedHint)
              const Text('缓存仅用于显示，不启用任务动作'),
          ],
        ),
      ),
    );
  }

  /// 单个泳道：backlog/todo 恒显标题；泳道头提供「+任务」「+块」（+块跟随能力）。
  Widget _buildLaneSection(
    ThemeData theme,
    String lane,
    List<AutomationBoardItem> items,
  ) {
    final alwaysVisible = lane == 'backlog' || lane == 'todo';
    if (items.isEmpty && !alwaysVisible) return const SizedBox.shrink();
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
              _badge(theme, '${items.length} 个任务'),
              const Spacer(),
              if (alwaysVisible) ...[
                TextButton.icon(
                  key: Key('automation-lane-add-$lane'),
                  onPressed: () => _openCreateDialog(preferredAction: lane),
                  icon: const Icon(Icons.add, size: 16),
                  label: const Text('任务'),
                ),
                const SizedBox(width: 4),
                TextButton.icon(
                  key: Key('automation-lane-add-block-$lane'),
                  onPressed: _canCreateTaskBlock
                      ? () => _openCreateDialog(
                            preferredAction: lane,
                            blockMode: true,
                          )
                      : null,
                  icon: const Icon(Icons.view_column, size: 16),
                  label: const Text('块'),
                ),
              ],
            ],
          ),
          for (final item in items)
            item.isBlock
                ? _buildBlockCard(theme, item)
                : _buildTaskCard(theme, item.item!),
          if (items.isEmpty)
            Padding(
              padding: const EdgeInsets.symmetric(vertical: 8),
              child: Text('暂无任务', style: theme.textTheme.bodySmall),
            ),
        ],
      ),
    );
  }

  /// 任务块卡片：块标题 + 成员数徽章 + 可展开成员列表（上移/下移/末尾追加）。
  Widget _buildBlockCard(ThemeData theme, AutomationBoardItem item) {
    final blockId = item.blockId!;
    final expanded = _expandedBlockIds.contains(blockId);
    final canReorder = automationCanReorderBlock(item.members);
    final canAppend = automationCanAppendToBlock(item.members);
    return Card(
      key: Key('automation-block-$blockId'),
      margin: const EdgeInsets.only(top: 8),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          ListTile(
            title: Text(item.title ?? blockId),
            subtitle: Text(expanded ? '收起任务块' : '展开任务块'),
            trailing: _badge(theme, '${item.members.length} 步'),
            onTap: () {
              setState(() {
                if (expanded) {
                  _expandedBlockIds.remove(blockId);
                } else {
                  _expandedBlockIds.add(blockId);
                }
              });
            },
          ),
          if (expanded) ...[
            for (var i = 0; i < item.members.length; i++) ...[
              if (canReorder)
                Padding(
                  padding: const EdgeInsets.fromLTRB(16, 4, 16, 0),
                  child: Row(
                    children: [
                      OutlinedButton(
                        key: Key('block-move-up-$blockId-$i'),
                        onPressed: i == 0
                            ? null
                            : () => _moveBlockMember(item, i, -1),
                        child: const Text('上移'),
                      ),
                      const SizedBox(width: 8),
                      OutlinedButton(
                        key: Key('block-move-down-$blockId-$i'),
                        onPressed: i == item.members.length - 1
                            ? null
                            : () => _moveBlockMember(item, i, 1),
                        child: const Text('下移'),
                      ),
                    ],
                  ),
                ),
              _buildTaskCard(theme, item.members[i]),
            ],
            if (canAppend)
              Padding(
                padding: const EdgeInsets.fromLTRB(16, 4, 16, 8),
                child: TextButton.icon(
                  key: Key('block-append-$blockId'),
                  onPressed: () => _openAppendDialog(blockId),
                  icon: const Icon(Icons.add, size: 16),
                  label: const Text('在末尾添加任务'),
                ),
              ),
          ],
        ],
      ),
    );
  }

  /// 任务行：标题 + 来源徽章 + workflow 徽章；点击展开详情卡；选中/聚焦高亮。
  Widget _buildTaskCard(ThemeData theme, AutomationRenderableTask renderable) {
    final task = renderable.task;
    final id = renderable.id;
    final title = task['title'] as String? ?? id;
    final goal = task['goal'] as String? ?? '';
    final origin = renderable.origin;
    final originLabel = origin == 'remote'
        ? '远端 ${renderable.deviceName ?? 'unknown'}'
        : '本机';
    final workflow = task['workflowState'] as String? ?? 'backlog';
    final selected = _selectedTaskId == id;
    return Card(
      key: Key('automation-task-$id'),
      margin: const EdgeInsets.only(top: 8),
      color: selected
          ? Color.alphaBlend(
              theme.colorScheme.primaryContainer.withAlpha(110),
              theme.colorScheme.surface,
            )
          : null,
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

  /// 任务详情：goal / 验收 / workflow / Run / 运行阶段 / 运行消息 / Claude Session /
  /// Transcript / 阻塞原因 + 打开执行现场 + Evidence 时间线。
  Widget _buildDetailSection(ThemeData theme, Map<String, dynamic> task) {
    final unknown = 'unknown';
    final attemptPhase = task['attemptPhase'] as String?;
    final blockedReason = task['blockedReason'] as String?;
    final worktreeId = task['worktreeId'] as String?;
    final sessionId = task['sessionId'] as String?;
    // 对齐 web：worktreeId 或 sessionId 任一非空即可打开执行现场（AND 改 OR），
    // 缺失一边原样传 null 由壳层回落。
    final canOpenExecutionContext =
        (worktreeId != null && worktreeId.isNotEmpty) ||
            (sessionId != null && sessionId.isNotEmpty);
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
            'Run',
            _runtimeValue(task['runState'] as String?, '') == ''
                ? unknown
                : _runStateLabels[task['runState'] as String?] ??
                    (task['runState'] as String? ?? unknown),
          ),
          _kvRow(
            theme,
            '运行阶段',
            attemptPhase == null || attemptPhase.isEmpty ? unknown : attemptPhase,
          ),
          _kvRow(
            theme,
            '运行消息',
            _runtimeValue(task['lastRuntimeMessage'] as String?, unknown),
          ),
          _kvRow(
            theme,
            'Claude Session',
            _runtimeValue(task['claudeSessionId'] as String?, unknown),
          ),
          _kvRow(
            theme,
            'Transcript',
            _runtimeValue(task['transcriptPath'] as String?, unknown),
          ),
          if (blockedReason != null && blockedReason.isNotEmpty)
            _kvRow(theme, '阻塞原因', blockedReason, danger: true),
          Padding(
            padding: const EdgeInsets.only(top: 8, bottom: 4),
            child: FilledButton.icon(
              key: const Key('automation-open-execution'),
              onPressed: canOpenExecutionContext
                  ? () => widget.onFocusSession?.call(worktreeId, sessionId)
                  : null,
              icon: const Icon(Icons.terminal, size: 16),
              label: Text(
                canOpenExecutionContext ? '打开执行现场' : '暂无执行现场',
              ),
            ),
          ),
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

  /// 离线待同步区：标题/状态/目标设备/错误；failed 行提供 重试 与 丢弃（需确认）；
  /// Attention 聚焦命中时整卡高亮。
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
    final focused = _focusedOutboxId != null && _focusedOutboxId == id;
    return Card(
      key: Key('automation-outbox-$id'),
      margin: const EdgeInsets.only(top: 8),
      color: focused
          ? Color.alphaBlend(
              theme.colorScheme.primaryContainer.withAlpha(110),
              theme.colorScheme.surface,
            )
          : null,
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

  /// 实验组区：组级进度 + candidate 状态 + 推荐 winner 信息；
  /// NeedsDecision 时提供「采纳推荐」「取消实验」；不展示 diff。
  Widget _buildExperimentsSection(ThemeData theme) {
    return Padding(
      padding: const EdgeInsets.only(top: 16),
      child: Column(
        key: const Key('automation-experiments'),
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Text('实验组', style: theme.textTheme.titleMedium),
              const SizedBox(width: 8),
              _badge(theme, '${_experiments.length} 组'),
            ],
          ),
          if (_experiments.isEmpty)
            const Padding(
              padding: EdgeInsets.symmetric(vertical: 8),
              child: Text('暂无实验组。可在创建弹窗中发起比较实验。'),
            ),
          for (final experiment in _experiments)
            _buildExperimentCard(theme, experiment),
        ],
      ),
    );
  }

  /// 单个实验组卡片：标题/状态/目标/推荐理由 + candidate 列表 + 决策动作。
  Widget _buildExperimentCard(ThemeData theme, Map<String, dynamic> experiment) {
    final id = experiment['id'] as String? ?? '';
    final title = experiment['title'] as String? ?? id;
    final status = experiment['status'] as String? ?? '';
    final goal = experiment['goal'] as String? ?? '';
    final selectionReason = experiment['selectionReason'] as String? ?? '';
    final candidates = asObjectList(experiment['candidates']);
    final needsDecision = automationExperimentNeedsDecision(experiment);
    final recommendedId = automationExperimentRecommendedTaskId(experiment);
    final busy = _experimentActionId != null;
    return Card(
      key: Key('automation-experiment-$id'),
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
                    title,
                    style: theme.textTheme.titleSmall
                        ?.copyWith(fontWeight: FontWeight.w600),
                  ),
                ),
                _badge(theme, status),
              ],
            ),
            if (goal.isNotEmpty)
              Padding(
                padding: const EdgeInsets.only(top: 4),
                child: Text(goal, style: theme.textTheme.bodySmall),
              ),
            if (selectionReason.isNotEmpty)
              Padding(
                padding: const EdgeInsets.only(top: 2),
                child: Text(
                  selectionReason,
                  key: Key('automation-experiment-reason-$id'),
                  style: theme.textTheme.bodySmall,
                ),
              ),
            for (final candidate in candidates)
              Padding(
                padding: const EdgeInsets.symmetric(vertical: 2),
                child: Row(
                  children: [
                    Expanded(
                      child: Text(
                        '#${_runtimeIntValue(candidate['ordinal'])} '
                        '${candidate['strategyLabel'] as String? ?? ''} · '
                        '${candidate['providerId'] as String? ?? ''}',
                        style: theme.textTheme.bodySmall,
                      ),
                    ),
                    _badge(theme, candidate['outcome'] as String? ?? ''),
                  ],
                ),
              ),
            if (needsDecision)
              Padding(
                padding: const EdgeInsets.only(top: 8),
                child: Row(
                  children: [
                    FilledButton.icon(
                      key: Key('automation-experiment-approve-$id'),
                      onPressed:
                          (recommendedId != null && !busy)
                              ? () => _approveExperiment(experiment)
                              : null,
                      icon: const Icon(Icons.check, size: 16),
                      label: const Text('采纳推荐'),
                    ),
                    const SizedBox(width: 8),
                    OutlinedButton(
                      key: Key('automation-experiment-cancel-$id'),
                      onPressed: busy ? null : () => _cancelExperiment(experiment),
                      child: const Text('取消实验'),
                    ),
                  ],
                ),
              ),
          ],
        ),
      ),
    );
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
            width: 96,
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
