import 'package:flutter/material.dart';

import '../address_book/book.dart';
import '../address_book/models.dart';
import '../core/lan_http.dart';
import '../files/workspace.dart';
import '../git/client.dart';
import '../projects/client.dart';
import '../workbench/nav.dart';
import '../workbench/worktree.dart';
import 'attention_page.dart';
import 'automation_page.dart';
import 'browser_page.dart';
import 'files_page.dart';
import 'git_page.dart';
import 'projects_page.dart';
import 'provider_page.dart';
import 'settings_page.dart';
import 'terminal_page.dart';
import 'transfer_page.dart';
import 'workbench_shell.dart';
import 'worktree_strip.dart';
import 'worktrees_page.dart';

/// Dual-mode workbench: global 项目/待处理/传输/设置/Provider,
/// project 终端/浏览器/文件/Git/worktrees/自动化.
class WorkbenchHome extends StatefulWidget {
  const WorkbenchHome({super.key, required this.book, required this.http});

  final AddressBook book;
  final LanHttpClient http;

  @override
  State<WorkbenchHome> createState() => _WorkbenchHomeState();
}

class _WorkbenchHomeState extends State<WorkbenchHome> {
  WorkbenchPanel _panel = WorkbenchPanel.projects;
  ProjectSummary? _project;
  String? _worktreeId;
  String? _sessionId;
  List<Map<String, dynamic>> _worktrees = [];
  final _files = FileWorkspaceController();

  ServerRecord get _server => widget.book.active!;

  WorkbenchNavMode get _mode =>
      resolveNavMode(_panel, _project != null);

  Future<void> _loadWorktrees(ProjectSummary project, {required bool projectChanged}) async {
    try {
      final body = await GitClient(widget.http, _server.baseUrl).listWorktrees(project.id);
      final trees = asObjectList(body, wrapKey: 'worktrees');
      if (!mounted) {
        return;
      }
      setState(() {
        _worktrees = trees;
        _worktreeId = resolveActiveWorktreeId(
          trees: trees,
          previousId: _worktreeId,
          projectChanged: projectChanged,
        );
      });
    } catch (_) {}
  }

  Future<bool> _confirmLeaveDirty(String nextWorktreeId) async {
    if (!_files.shouldBlockContextSwitch(
      projectId: _project?.id ?? '',
      worktreeId: nextWorktreeId,
    )) {
      return true;
    }
    final choice = await showDialog<String>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('未保存的文件'),
        content: const Text('切换工作区前请保存或丢弃当前文件。'),
        actions: [
          TextButton(onPressed: () => Navigator.pop(context, 'cancel'), child: const Text('取消')),
          TextButton(onPressed: () => Navigator.pop(context, 'discard'), child: const Text('丢弃')),
        ],
      ),
    );
    if (choice == 'discard') {
      _files.markClean();
      return true;
    }
    return false;
  }

  void _openProject(ProjectSummary project, {WorkbenchPanel panel = WorkbenchPanel.terminal, String? sessionId}) {
    final projectChanged = _project?.id != project.id;
    setState(() {
      _project = project;
      _panel = panel;
      _sessionId = sessionId;
      if (projectChanged) {
        _worktreeId = clearWorktreeOnLeaveProject();
        _worktrees = [];
      }
    });
    _loadWorktrees(project, projectChanged: projectChanged);
  }

  void _select(WorkbenchPanel next) {
    final panel = selectPanelForProject(hasProject: _project != null, next: next);
    setState(() => _panel = panel);
  }

  Widget _body() {
    switch (_panel) {
      case WorkbenchPanel.projects:
        return ProjectsPage(
          book: widget.book,
          http: widget.http,
          onOpen: _openProject,
        );
      case WorkbenchPanel.attention:
        return AttentionPage(
          book: widget.book,
          http: widget.http,
          onNavigate: (project, panel, sessionId) {
            _openProject(
              project,
              panel: parseWorkbenchPanel(panel) ?? WorkbenchPanel.terminal,
              sessionId: sessionId,
            );
          },
        );
      case WorkbenchPanel.transfer:
        return TransferPage(book: widget.book, http: widget.http);
      case WorkbenchPanel.settings:
        return SettingsPage(book: widget.book, http: widget.http);
      case WorkbenchPanel.provider:
        return ProviderPage(book: widget.book, http: widget.http);
      case WorkbenchPanel.terminal:
        return TerminalPage(
          key: ValueKey('terminal-${_project!.id}-$_worktreeId-$_sessionId'),
          book: widget.book,
          http: widget.http,
          project: _project!,
          preferredSessionId: _sessionId,
          worktreeId: _worktreeId,
        );
      case WorkbenchPanel.files:
        return FilesPage(
          key: ValueKey('files-${_project!.id}-$_worktreeId'),
          book: widget.book,
          http: widget.http,
          project: _project!,
          worktreeId: _worktreeId,
          workspace: _files,
        );
      case WorkbenchPanel.git:
        return GitPage(
          key: ValueKey('git-${_project!.id}-$_worktreeId'),
          book: widget.book,
          http: widget.http,
          project: _project!,
          worktreeId: _worktreeId,
        );
      case WorkbenchPanel.worktrees:
        return WorktreesPage(
          book: widget.book,
          http: widget.http,
          project: _project!,
          activeId: _worktreeId,
          onSelect: (tree) async {
            final id = tree['id'] as String? ?? '';
            if (!await _confirmLeaveDirty(id)) {
              return;
            }
            setState(() => _worktreeId = id);
          },
        );
      case WorkbenchPanel.automation:
        return AutomationPage(
          book: widget.book,
          http: widget.http,
          project: _project!,
        );
      case WorkbenchPanel.browser:
        return BrowserPage(
          key: ValueKey('browser-${_project!.id}-$_worktreeId'),
          book: widget.book,
          http: widget.http,
          project: _project!,
          worktreeId: _worktreeId,
        );
    }
  }

  @override
  Widget build(BuildContext context) {
    final label = _server.name.isNotEmpty
        ? _server.name
        : (_server.deviceName ?? _server.baseUrl);
    return WorkbenchShell(
      mode: _mode,
      panel: _panel,
      projectLabel: _project?.name ?? label,
      subtitle: _server.baseUrl,
      onSelect: _select,
      onBackToProjects: () {
        setState(() {
          _panel = WorkbenchPanel.projects;
          _project = null;
          _sessionId = null;
          _worktreeId = clearWorktreeOnLeaveProject();
          _worktrees = [];
        });
      },
      worktreeStrip: _project != null && shouldShowWorktreeStrip(_panel)
          ? WorktreeStrip(
              worktrees: _worktrees,
              activeId: _worktreeId,
              onSelect: (tree) async {
                final id = tree['id'] as String? ?? '';
                if (!await _confirmLeaveDirty(id)) {
                  return;
                }
                setState(() => _worktreeId = id);
              },
            )
          : null,
      child: _body(),
    );
  }
}
