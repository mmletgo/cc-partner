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

/// Business Logic: 文件大小展示与 web MobileFilesPanel 相同分级：
/// <1KB 精确到 B，KB/MB 各保留一位小数。
/// Code Logic: null 返回 null 由调用方占位；1024 进制换算。
String? formatFileSizeLabel(int? size) {
  if (size == null) {
    return null;
  }
  if (size < 1024) {
    return '$size B';
  }
  if (size < 1024 * 1024) {
    return '${(size / 1024).toStringAsFixed(1)} KB';
  }
  return '${(size / 1024 / 1024).toStringAsFixed(1)} MB';
}

/// Business Logic: 文件修改时间需按用户本地时间展示；后端给 ISO 字符串，
/// 无效值不能让预览页崩溃。
/// Code Logic: 解析失败或缺省返回 null（调用方显示占位符），成功转本地 `y-M-d HH:mm`。
String? formatFileModifiedAt(String? iso) {
  if (iso == null || iso.isEmpty) {
    return null;
  }
  final parsed = DateTime.tryParse(iso);
  if (parsed == null) {
    return null;
  }
  final local = parsed.isUtc ? parsed.toLocal() : parsed;
  String two(int value) => value.toString().padLeft(2, '0');
  return '${local.year}-${local.month}-${two(local.day)} '
      '${two(local.hour)}:${two(local.minute)}';
}

/// Business Logic: 打开文件后要在头部展示「类型 · 大小 · 修改时间」元信息行，
/// 与 web metadataText 同源同格式（`files/open` 响应）。
/// Code Logic: 宽容解析 detectedType 与 metadata.size/modifiedAt；
/// 单段缺省用「—」占位；三段全缺省返回 null（不渲染该行）。
String? fileMetadataText(Map<String, dynamic> opened) {
  final metadata = opened['metadata'];
  final metadataMap =
      metadata is Map ? Map<String, dynamic>.from(metadata) : const <String, dynamic>{};
  final detectedType = (opened['detectedType'] as String?)?.trim() ?? '';
  final size = (metadataMap['size'] as num?)?.toInt();
  final modifiedAt = metadataMap['modifiedAt'] as String?;
  final sizeLabel = formatFileSizeLabel(size);
  final modifiedLabel = formatFileModifiedAt(modifiedAt);
  final parts = [
    detectedType.isNotEmpty ? detectedType : '—',
    sizeLabel ?? '—',
    modifiedLabel ?? '—',
  ];
  if (parts.every((part) => part == '—')) {
    return null;
  }
  return parts.join(' · ');
}

/// Business Logic: 保存文本的乐观锁基线来自 open 响应 `text.baseHash`（后端
/// WorkbenchTextContent 的 serde camelCase 字段）；旧注入/旧网关可能仍给 `hash`，
/// 读错字段会导致保存恒带空基线而必然 409。
/// Code Logic: 优先读 `baseHash`，兼容回退 `hash`；缺省或空串返回空字符串。
String fileTextBaseHash(Map<String, dynamic> text) {
  final baseHash = text['baseHash'];
  if (baseHash is String && baseHash.isNotEmpty) {
    return baseHash;
  }
  final hash = text['hash'];
  if (hash is String && hash.isNotEmpty) {
    return hash;
  }
  return '';
}

/// Business Logic: CSV 只读预览的表头字段在后端 DTO（WorkbenchCsvPreview）是
/// `columns`；读错成不存在字段会让表头恒为占位列。
/// Code Logic: 优先 `columns`，兼容回退旧字段 `headers`，都缺省给 `['col']` 占位。
List<String> csvPreviewColumns(Map<String, dynamic> csv) {
  for (final key in const ['columns', 'headers']) {
    final value = csv[key];
    if (value is List && value.isNotEmpty) {
      return [for (final item in value) '$item'];
    }
  }
  return const ['col'];
}

/// Business Logic: open 响应的 notice（如哈希不匹配、权限受限）必须上屏提示。
/// Code Logic: 宽容读取 notice 字符串，空串视为无提示。
String? openFileNotice(Map<String, dynamic> opened) {
  final notice = (opened['notice'] as String?)?.trim() ?? '';
  return notice.isEmpty ? null : notice;
}

/// Business Logic: open 响应 truncated 表示内容被截断展示，避免用户误以为全文。
/// Code Logic: 宽容读取布尔字段。
bool openFileTruncated(Map<String, dynamic> opened) => opened['truncated'] == true;

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

  /// 保存委托：由持有草稿内容与保存通道的 FilePreviewPage 在草稿变 dirty 时注册；
  /// 壳层「切换项目」三选确认中的「保存」经此执行真实保存（无页面注册时为 null）。
  Future<bool> Function()? saveHandler;

  /// 是否存在可执行的保存委托（决定三选确认是否展示「保存」项）。
  bool get canSave => saveHandler != null;

  /// Business Logic: 壳层在用户选择「保存并切换」时需要真正把草稿写回后端，
  /// 但壳层拿不到草稿内容，必须委托给注册了保存通道的预览页。
  /// Code Logic: 调用注册的委托并透传其成败；无委托时返回 false（调用方应中止切换）。
  Future<bool> save() async {
    final handler = saveHandler;
    if (handler == null) {
      return false;
    }
    return handler();
  }

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
