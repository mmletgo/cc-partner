import 'server_url.dart';

/// Accept desktop QR content (`http://ip:port/mobile`) or a raw host:port.
/// Returns the original trimmed payload if it parses as a PC entry; otherwise null.
String? qrPayloadToServerInput(String? raw) {
  if (raw == null) {
    return null;
  }
  final trimmed = raw.trim();
  if (trimmed.isEmpty) {
    return null;
  }
  // Desktop QR is an http(s) URL. Also accept a compact host[:port] without spaces.
  final looksLikeUrl = trimmed.contains('://');
  final looksLikeHostPort = RegExp(r'^[A-Za-z0-9._\-\[\]]+(?::\d{1,5})?$').hasMatch(trimmed);
  if (!looksLikeUrl && !looksLikeHostPort) {
    return null;
  }
  try {
    parseServerInput(trimmed);
    return trimmed;
  } on FormatException {
    return null;
  }
}
