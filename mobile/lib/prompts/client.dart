import '../core/lan_http.dart';

class FavoritePrompt {
  const FavoritePrompt({
    required this.id,
    required this.title,
    required this.content,
  });

  final String id;
  final String title;
  final String content;

  factory FavoritePrompt.fromJson(Map<String, dynamic> json) => FavoritePrompt(
        id: json['id'] as String? ?? '',
        title: json['title'] as String? ?? json['name'] as String? ?? '',
        content: json['content'] as String? ?? json['body'] as String? ?? '',
      );
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
