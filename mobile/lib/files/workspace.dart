enum FileKind { code, markdown, html, image, csv, sqlite, other }

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
