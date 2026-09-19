import 'package:flutter/material.dart';

import '../address_book/book.dart';
import '../core/lan_http.dart';
import '../projects/client.dart';
import '../transfer/api.dart';
import 'project_dir_picker.dart';

/// Workbench 项目列表页：最近项目 + 添加本机 / 局域网目录（目录浏览式选择器）。
class ProjectsPage extends StatefulWidget {
  const ProjectsPage({
    super.key,
    required this.book,
    required this.http,
    required this.onOpen,
    this.onProjectRemoved,
    this.client,
    this.transferApi,
  });

  final AddressBook book;
  final LanHttpClient http;
  final void Function(ProjectSummary project) onOpen;

  /// 删除项目成功后回调（接缝契约：参数为已删除项目 id）；
  /// 壳层据此在删除的是激活项目时清空工作台上下文。
  final void Function(String projectId)? onProjectRemoved;

  /// 测试注入的项目接口；为空时按当前主机构造。
  final ProjectsClient? client;

  /// 测试注入的设备列表接口；为空时按当前主机构造。
  final TransferApi? transferApi;

  @override
  State<ProjectsPage> createState() => _ProjectsPageState();
}

class _ProjectsPageState extends State<ProjectsPage> {
  late final ProjectsClient _client;
  late final TransferApi _transfer;
  final Set<String> _removingIds = {};
  List<ProjectSummary> _items = [];

  /// Agent Fleet 摘要；数据不可得时为 null（整行隐藏不占位）。
  LanFleetOverview? _fleet;
  String? _error;
  bool _loading = true;

  @override
  void initState() {
    super.initState();
    final baseUrl = widget.book.active?.baseUrl ?? '';
    _client = widget.client ?? ProjectsClient(widget.http, baseUrl);
    _transfer = widget.transferApi ?? TransferApi(widget.http, baseUrl);
    _reload();
    _loadFleet();
  }

  /// Business Logic: 项目列表需要一条跨设备 Agent 异常/离线摘要（对齐 web
  /// MobileProjectPanel 的 fleetSummary 行），失败时静默隐藏，不阻塞项目列表。
  /// Code Logic: 拉 lan-fleet 快照并解析为 LanFleetOverview；任何异常保持 null。
  Future<void> _loadFleet() async {
    try {
      final snapshot = await _client.fleetSnapshot();
      if (!mounted) {
        return;
      }
      setState(() => _fleet = LanFleetOverview.fromSnapshot(snapshot));
    } catch (_) {
      // Fleet 数据不可得：整行隐藏不占位。
    }
  }

  Future<void> _reload() async {
    setState(() {
      _loading = true;
      _error = null;
    });
    try {
      final items = await _client.listRecent();
      if (mounted) {
        setState(() {
          _items = items;
          _loading = false;
        });
      }
    } catch (error) {
      if (mounted) {
        setState(() {
          _error = error.toString();
          _loading = false;
        });
      }
    }
  }

  /// 打开目录浏览式选择器；成功打开项目后回传给上层导航。
  Future<void> _openPicker({required bool lan}) async {
    final opened = await showProjectDirPicker(
      context,
      client: _client,
      transferApi: _transfer,
      lan: lan,
    );
    if (opened != null) {
      widget.onOpen(opened);
    }
  }

  /// 先弹确认框，确认后才从最近列表移除；移除中按钮禁用防重复提交。
  Future<void> _remove(ProjectSummary project) async {
    if (_removingIds.contains(project.id)) {
      return;
    }
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        key: const Key('remove-confirm'),
        title: Text('移除项目「${project.name}」？'),
        content: const Text('只会从这台电脑的最近列表移除，不会删除文件。'),
        actions: [
          TextButton(
            key: const Key('remove-confirm-cancel'),
            onPressed: () => Navigator.pop(context, false),
            child: const Text('取消'),
          ),
          FilledButton(
            key: const Key('remove-confirm-accept'),
            onPressed: () => Navigator.pop(context, true),
            child: const Text('移除'),
          ),
        ],
      ),
    );
    if (confirmed != true || !mounted) {
      return;
    }
    setState(() => _removingIds.add(project.id));
    try {
      await _client.remove(project.id);
      // 接缝契约：删除成功后通知壳层（用于清理激活项目的工作台上下文）。
      widget.onProjectRemoved?.call(project.id);
      await _reload();
    } catch (error) {
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(
          SnackBar(content: Text('$error')),
        );
      }
    } finally {
      if (mounted) {
        setState(() => _removingIds.remove(project.id));
      }
    }
  }

  /// Business Logic: 列表加载失败时不能只给一行裸错误——用户需要明确的失败卡与重试入口，
  /// 且下拉刷新在错误态同样可用（对齐 web projectPanel 的 error + reload 语义）。
  /// Code Logic: 错误卡展示「加载失败」+ 具体原因 + 重试按钮；RefreshIndicator 包住
  /// 错误列表允许下拉重载。
  Widget _errorPanel() {
    final theme = Theme.of(context);
    return RefreshIndicator(
      onRefresh: _reload,
      child: ListView(
        physics: const AlwaysScrollableScrollPhysics(),
        children: [
          Card(
            key: const Key('projects-error-card'),
            margin: const EdgeInsets.all(16),
            child: Padding(
              padding: const EdgeInsets.all(16),
              child: Column(
                mainAxisSize: MainAxisSize.min,
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Row(
                    children: [
                      Icon(Icons.error_outline, color: theme.colorScheme.error),
                      const SizedBox(width: 8),
                      Text('加载失败', style: theme.textTheme.titleMedium),
                    ],
                  ),
                  const SizedBox(height: 8),
                  Text(
                    _error ?? '',
                    style: theme.textTheme.bodySmall,
                  ),
                  const SizedBox(height: 12),
                  FilledButton.icon(
                    key: const Key('projects-error-retry'),
                    onPressed: _reload,
                    icon: const Icon(Icons.refresh),
                    label: const Text('重试'),
                  ),
                ],
              ),
            ),
          ),
        ],
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    final fleet = _fleet;
    return Column(
      children: [
        Padding(
          padding: const EdgeInsets.all(8),
          child: Row(
            children: [
              FilledButton(
                key: const Key('project-add-local'),
                onPressed: () => _openPicker(lan: false),
                child: const Text('添加本机目录'),
              ),
              const SizedBox(width: 8),
              OutlinedButton(
                key: const Key('project-add-lan'),
                onPressed: () => _openPicker(lan: true),
                child: const Text('添加局域网目录'),
              ),
            ],
          ),
        ),
        if (fleet != null)
          Padding(
            padding: const EdgeInsets.fromLTRB(12, 0, 12, 4),
            child: Align(
              alignment: Alignment.centerLeft,
              child: Text(
                fleet.label,
                key: const Key('projects-fleet-summary'),
                style: Theme.of(context).textTheme.bodySmall,
              ),
            ),
          ),
        Expanded(
          child: _loading
              ? const Center(child: CircularProgressIndicator())
              : _error != null
                  ? _errorPanel()
                  : _items.isEmpty
                      ? RefreshIndicator(
                          onRefresh: _reload,
                          child: ListView(
                            physics: const AlwaysScrollableScrollPhysics(),
                            children: const [
                              SizedBox(height: 120),
                              Center(child: Text('还没有项目文件夹')),
                            ],
                          ),
                        )
                      : RefreshIndicator(
                          onRefresh: _reload,
                          child: ListView.builder(
                            physics: const AlwaysScrollableScrollPhysics(),
                            itemCount: _items.length,
                            itemBuilder: (context, index) {
                              final project = _items[index];
                              final removing = _removingIds.contains(project.id);
                              return ListTile(
                                leading: const Icon(Icons.folder),
                                title: Text(project.name),
                                subtitle: Text(project.path ?? project.kind ?? project.id),
                                onTap: () => widget.onOpen(project),
                                trailing: IconButton(
                                  key: Key('project-remove-${project.id}'),
                                  icon: removing
                                      ? const SizedBox(
                                          width: 16,
                                          height: 16,
                                          child: CircularProgressIndicator(strokeWidth: 2),
                                        )
                                      : const Icon(Icons.delete_outline),
                                  onPressed: removing ? null : () => _remove(project),
                                ),
                              );
                            },
                          ),
                        ),
        ),
      ],
    );
  }
}
