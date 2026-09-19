import 'dart:convert';
import 'dart:typed_data';

enum FileKind { code, markdown, html, image, csv, sqlite, other }

/// Decode a Workbench `files/open` image DTO into bytes for [Image.memory].
/// data: URLs cannot be loaded with [Image.network].
Uint8List? imageBytesFromOpenFile(Map<String, dynamic> opened) {
  final image = opened['image'];
  if (image is! Map) {
    return null;
  }
  final map = Map<String, dynamic>.from(image);
  final raw = map['base64'] as String?;
  if (raw != null && raw.isNotEmpty) {
    try {
      return Uint8List.fromList(base64Decode(raw));
    } catch (_) {
      return null;
    }
  }
  final dataUrl = map['dataUrl'] as String?;
  if (dataUrl == null || dataUrl.isEmpty) {
    return null;
  }
  return decodeDataUrlImageBytes(dataUrl);
}

Uint8List? decodeDataUrlImageBytes(String dataUrl) {
  if (!dataUrl.startsWith('data:')) {
    return null;
  }
  final comma = dataUrl.indexOf(',');
  if (comma < 0 || comma >= dataUrl.length - 1) {
    return null;
  }
  try {
    return Uint8List.fromList(base64Decode(dataUrl.substring(comma + 1)));
  } catch (_) {
    return null;
  }
}

/// data: and raw base64 images are rendered from bytes, never via network.
bool imagePreviewUsesNetworkUrl(Map<String, dynamic> opened) {
  return imageBytesFromOpenFile(opened) == null;
}

const kMarkdownPreviewModes = ['source', 'render', 'split'];

class SqlitePreviewState {
  const SqlitePreviewState({
    required this.tables,
    required this.selectedTable,
    required this.rows,
  });

  final List<String> tables;
  final String? selectedTable;
  final List<dynamic> rows;

  factory SqlitePreviewState.fromOpen(Map<String, dynamic> sqlite) {
    final tables = (sqlite['tables'] as List<dynamic>? ?? const [])
        .map((e) => '$e')
        .toList();
    final selected = sqlite['table'] as String? ?? sqlite['selectedTable'] as String?;
    final rows = sqlite['rows'] as List<dynamic>? ?? const [];
    return SqlitePreviewState(
      tables: tables,
      selectedTable: selected ?? (tables.isEmpty ? null : tables.first),
      rows: rows,
    );
  }

  SqlitePreviewState selectTable(String table) => SqlitePreviewState(
        tables: tables,
        selectedTable: table,
        rows: const [],
      );
}

FileKind detectFileKind(String path, {String? mime}) {
  final lower = path.toLowerCase();
  if (mime != null && mime.startsWith('image/')) {
    return FileKind.image;
  }
  if (lower.endsWith('.md') || lower.endsWith('.markdown')) {
    return FileKind.markdown;
  }
  if (lower.endsWith('.html') || lower.endsWith('.htm')) {
    return FileKind.html;
  }
  if (lower.endsWith('.csv')) {
    return FileKind.csv;
  }
  if (lower.endsWith('.sqlite') || lower.endsWith('.db') || lower.endsWith('.sqlite3')) {
    return FileKind.sqlite;
  }
  const codeExt = [
    '.dart',
    '.rs',
    '.ts',
    '.tsx',
    '.js',
    '.json',
    '.py',
    '.go',
    '.java',
    '.kt',
    '.c',
    '.h',
    '.cpp',
    '.yml',
    '.yaml',
    '.toml',
    '.css',
    '.xml',
    '.sh',
  ];
  if (codeExt.any(lower.endsWith)) {
    return FileKind.code;
  }
  return FileKind.other;
}

class FileDirtySnapshot {
  const FileDirtySnapshot({
    required this.dirty,
    this.projectId,
    this.worktreeId,
    this.path,
  });

  final bool dirty;
  final String? projectId;
  final String? worktreeId;
  final String? path;
}

/// Blocks project/worktree switch while a file is dirty unless the user saves or discards.
class FileWorkspaceController {
  FileDirtySnapshot snapshot = const FileDirtySnapshot(dirty: false);

  void markDirty({
    required String projectId,
    required String worktreeId,
    required String path,
  }) {
    snapshot = FileDirtySnapshot(
      dirty: true,
      projectId: projectId,
      worktreeId: worktreeId,
      path: path,
    );
  }

  void markClean() {
    snapshot = const FileDirtySnapshot(dirty: false);
  }

  bool shouldBlockContextSwitch({
    required String projectId,
    required String worktreeId,
  }) {
    if (!snapshot.dirty) {
      return false;
    }
    return snapshot.projectId != projectId || snapshot.worktreeId != worktreeId;
  }
}
