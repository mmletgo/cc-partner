/// Default LAN HTTP port when the user omits it (cc-partner preferred port).
const int kDefaultLanPort = 62116;

/// Parsed PC entry: host + port, never a `/mobile` path.
class ParsedServer {
  const ParsedServer({required this.host, required this.port});

  final String host;
  final int port;

  /// Canonical LAN base URL without trailing slash or path.
  ///
  /// Business Logic: 地址簿只存 host:port，探测与请求都走这一条 baseUrl。
  /// Code Logic: IPv6 必须加方括号，否则 `http://fd7a::1:62116` 无法解析。
  String get baseUrl {
    final hostPart = host.contains(':') ? '[$host]' : host;
    return 'http://$hostPart:$port';
  }
}

/// Parse hand-typed host:port, pasted URL, or desktop QR (`http://ip:port/mobile`).
///
/// Business Logic: the App stores PC entries, not browser paths.
/// Code Logic: accept URL or host[:port]; drop any path; default port 62116;
/// lowercase host; reject empty host.
ParsedServer parseServerInput(String raw) {
  final trimmed = raw.trim();
  if (trimmed.isEmpty) {
    throw const FormatException('empty server address');
  }

  if (trimmed.contains('://')) {
    final uri = Uri.parse(trimmed);
    if (uri.host.isEmpty) {
      throw FormatException('missing host in $trimmed');
    }
    final port = uri.hasPort ? uri.port : kDefaultLanPort;
    if (port <= 0 || port > 65535) {
      throw FormatException('invalid port $port');
    }
    return ParsedServer(host: uri.host.toLowerCase(), port: port);
  }

  String hostPart = trimmed;
  int port = kDefaultLanPort;
  if (trimmed.startsWith('[')) {
    final end = trimmed.indexOf(']');
    if (end <= 1) {
      throw FormatException('invalid IPv6 address $trimmed');
    }
    hostPart = trimmed.substring(1, end);
    final rest = trimmed.substring(end + 1);
    if (rest.startsWith(':')) {
      port = _parsePort(rest.substring(1));
    } else if (rest.isNotEmpty) {
      throw FormatException('invalid IPv6 address $trimmed');
    }
  } else {
    final colon = trimmed.lastIndexOf(':');
    if (colon > 0 && !trimmed.contains('::')) {
      hostPart = trimmed.substring(0, colon);
      port = _parsePort(trimmed.substring(colon + 1));
    }
  }

  if (hostPart.isEmpty) {
    throw const FormatException('empty host');
  }
  return ParsedServer(host: hostPart.toLowerCase(), port: port);
}

int _parsePort(String raw) {
  final port = int.tryParse(raw);
  if (port == null || port <= 0 || port > 65535) {
    throw FormatException('invalid port $raw');
  }
  return port;
}
