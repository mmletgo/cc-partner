/// Desktop WorkbenchHtmlPreview policy: empty sandbox, no scripts.
class HtmlPreviewPolicy {
  static const scriptsEnabled = false;
  static const referrerPolicy = 'no-referrer';
  static const modes = ['source', 'wysiwyg', 'split'];
}

/// Rewrite same-directory relative src/href to data URLs (css/img).
String rewriteRelativeAssetsToDataUrls(
  String html, {
  required String Function(String relativePath) loadAssetDataUrl,
}) {
  final pattern = RegExp(
    r'''(src|href)\s*=\s*(['"])(?!https?:|data:|//|#)([^'"]+)\2''',
    caseSensitive: false,
  );
  return html.replaceAllMapped(pattern, (match) {
    final attr = match.group(1)!;
    final quote = match.group(2)!;
    final path = match.group(3)!;
    final dataUrl = loadAssetDataUrl(path);
    return '$attr=$quote$dataUrl$quote';
  });
}
