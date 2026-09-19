import 'package:flutter/material.dart';

import '../address_book/book.dart';
import '../core/lan_http.dart';
import '../projects/client.dart';
import 'files_page.dart';
import 'git_page.dart';
import 'terminal_page.dart';

class ProjectHome extends StatefulWidget {
  const ProjectHome({
    super.key,
    required this.book,
    required this.http,
    required this.project,
    this.initialTab = 0,
    this.sessionId,
  });

  final AddressBook book;
  final LanHttpClient http;
  final ProjectSummary project;
  final int initialTab;
  final String? sessionId;

  @override
  State<ProjectHome> createState() => _ProjectHomeState();
}

class _ProjectHomeState extends State<ProjectHome> {
  late int _index;

  @override
  void initState() {
    super.initState();
    _index = widget.initialTab;
  }

  @override
  Widget build(BuildContext context) {
    final pages = [
      TerminalPage(
        book: widget.book,
        http: widget.http,
        project: widget.project,
        preferredSessionId: widget.sessionId,
      ),
      FilesPage(book: widget.book, http: widget.http, project: widget.project),
      GitPage(book: widget.book, http: widget.http, project: widget.project),
    ];
    return Scaffold(
      appBar: AppBar(title: Text(widget.project.name)),
      body: pages[_index],
      bottomNavigationBar: NavigationBar(
        selectedIndex: _index,
        onDestinationSelected: (value) => setState(() => _index = value),
        destinations: const [
          NavigationDestination(icon: Icon(Icons.terminal), label: '终端'),
          NavigationDestination(icon: Icon(Icons.insert_drive_file_outlined), label: '文件'),
          NavigationDestination(icon: Icon(Icons.merge_type), label: 'Git'),
        ],
      ),
    );
  }
}
