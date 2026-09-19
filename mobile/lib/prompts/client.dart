import '../core/lan_http.dart';

class FavoritePrompt {
  const FavoritePrompt({
    required this.id,
    required this.title,
    required this.content,
    this.tags = const [],
  });

  final String id;
  final String title;
  final String content;

  /// 标签数组；后端 legacy 单值 tag 字段仅作为其投影，宽容解析为单元素列表。
  final List<String> tags;

  factory FavoritePrompt.fromJson(Map<String, dynamic> json) {
    final tagList = (json['tags'] as List?)?.whereType<String>().toList() ?? const [];
    final legacyTag = json['tag'] as String?;
    return FavoritePrompt(
      id: json['id'] as String? ?? '',
      title: json['title'] as String? ?? json['name'] as String? ?? '',
      content: json['content'] as String? ?? json['body'] as String? ?? '',
      tags: tagList.isEmpty && legacyTag != null && legacyTag.isNotEmpty
          ? [legacyTag]
          : tagList,
    );
  }
}

/// 业务逻辑：收藏面板的标签筛选行需要从收藏列表派生标签集（对齐 web deriveTagsFromPrompts）。
///
/// Code Logic：收集所有非空 tag 去重后按字典序（localeCompare 语义）排序返回。
List<String> deriveTagsFromFavoritePrompts(List<FavoritePrompt> prompts) {
  final tags = <String>{};
  for (final prompt in prompts) {
    for (final tag in prompt.tags) {
      if (tag.isNotEmpty) {
        tags.add(tag);
      }
    }
  }
  final sorted = tags.toList()..sort((a, b) => a.compareTo(b));
  return sorted;
}

/// 业务逻辑：收藏条目要同时按标签与搜索词（title/content 子串）过滤，条件组合必须可单测。
///
/// Code Logic：selectedTag == allTagSentinel 或条目含该标签时通过；query 非空时要求
/// title 或 content 包含（不区分大小写）。返回过滤后的新列表。
List<FavoritePrompt> filterFavoritePrompts(
  List<FavoritePrompt> prompts, {
  required String selectedTag,
  required String allTagSentinel,
  required String query,
}) {
  final lower = query.trim().toLowerCase();
  return prompts.where((prompt) {
    if (selectedTag != allTagSentinel && !prompt.tags.contains(selectedTag)) {
      return false;
    }
    if (lower.isEmpty) {
      return true;
    }
    return prompt.title.toLowerCase().contains(lower) ||
        prompt.content.toLowerCase().contains(lower);
  }).toList();
}

class PromptsClient {
  PromptsClient(this._http, this.baseUrl);

  final LanHttpClient _http;
  final String baseUrl;

  Future<List<FavoritePrompt>> listFavorites() async {
    final body = await _http.getDynamic(
      baseUrl,
      '/api/mobile/prompts?favorite=true',
    );
    return asObjectList(body, wrapKey: 'prompts')
        .map(FavoritePrompt.fromJson)
        .toList();
  }

  Future<Map<String, dynamic>> streamOptimizerToSession({
    required String prompt,
    required String sessionId,
    String? workingDirectory,
    String targetLanguage = 'zh',
  }) {
    return _http.postJson(
      baseUrl,
      '/api/mobile/workbench/prompt-optimizer/stream-to-session',
      {
        'prompt': prompt,
        'sessionId': sessionId,
        'workingDirectory': workingDirectory,
        'targetLanguage': targetLanguage,
      },
    );
  }
}
