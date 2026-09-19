import 'dart:convert';
import 'dart:math';

import '../core/server_url.dart';
import 'models.dart';

/// Persistence for the address book JSON blob.
abstract class AddressBookStore {
  Future<String?> read();
  Future<void> write(String contents);
}

/// In-memory store used by tests.
class MemoryAddressBookStore implements AddressBookStore {
  String? value;

  @override
  Future<String?> read() async => value;

  @override
  Future<void> write(String contents) async {
    value = contents;
  }
}

/// Probe `GET /api/health` for one base URL.
typedef HealthProbe = Future<HealthSnapshot> Function(String baseUrl);

/// Thrown when health fails and the caller did not force-save.
class ServerUnreachableException implements Exception {
  ServerUnreachableException(this.baseUrl);
  final String baseUrl;
  @override
  String toString() => 'PC unreachable: $baseUrl';
}

/// Local address book: many PCs, one active workbench target.
class AddressBook {
  AddressBook({
    required AddressBookStore store,
    String? mobileDeviceId,
    Random? random,
  })  : _store = store,
        mobileDeviceId = mobileDeviceId ?? _newId(random ?? Random()),
        _random = random ?? Random();

  final AddressBookStore _store;
  final Random _random;
  final String mobileDeviceId;
  final List<ServerRecord> servers = [];
  String? activeServerId;
  void Function()? onDisconnectActive;

  ServerRecord? get active =>
      servers.where((s) => s.id == activeServerId).firstOrNull;

  /// Load JSON from the store. Missing/empty file is a valid empty book.
  Future<void> load() async {
    final raw = await _store.read();
    if (raw == null || raw.trim().isEmpty) {
      return;
    }
    final map = jsonDecode(raw) as Map<String, dynamic>;
    servers
      ..clear()
      ..addAll(
        (map['servers'] as List<dynamic>? ?? const []).map(
          (e) => ServerRecord.fromJson(e as Map<String, dynamic>),
        ),
      );
    activeServerId = map['activeServerId'] as String?;
  }

  Future<void> persist() async {
    await _store.write(
      jsonEncode({
        'mobileDeviceId': mobileDeviceId,
        'activeServerId': activeServerId,
        'servers': servers.map((s) => s.toJson()).toList(),
      }),
    );
  }

  /// Add or update from typed text / pasted URL / QR. Probe health first.
  Future<ServerRecord> addFromInput(
    String input, {
    required HealthProbe probe,
    bool forceIfUnreachable = false,
    String name = '',
  }) async {
    final parsed = parseServerInput(input);
    HealthSnapshot? snapshot;
    var health = ServerHealth.unreachable;
    try {
      snapshot = await probe(parsed.baseUrl);
      if (snapshot.ok) {
        health = ServerHealth.online;
      } else if (!forceIfUnreachable) {
        throw ServerUnreachableException(parsed.baseUrl);
      }
    } catch (error) {
      if (error is ServerUnreachableException) {
        rethrow;
      }
      if (!forceIfUnreachable) {
        throw ServerUnreachableException(parsed.baseUrl);
      }
    }

    final existing = servers.cast<ServerRecord?>().firstWhere(
          (s) => s!.host == parsed.host && s.port == parsed.port,
          orElse: () => null,
        );
    final record = existing ??
        ServerRecord(
          id: _newId(_random),
          host: parsed.host,
          port: parsed.port,
          baseUrl: parsed.baseUrl,
        );
    if (existing == null) {
      servers.add(record);
    }
    if (name.isNotEmpty) {
      record.name = name;
    }
    record.lastHealth = health;
    if (snapshot != null && snapshot.ok) {
      record.pcDeviceId = snapshot.deviceId;
      record.deviceName = snapshot.deviceName;
      record.protocolVersion = snapshot.protocolVersion;
      record.capabilities = List<String>.from(snapshot.capabilities);
    }
    activeServerId ??= record.id;
    await persist();
    return record;
  }

  /// Switch the workbench target. Tears the previous connection only.
  void switchActive(String id) {
    if (!servers.any((s) => s.id == id)) {
      throw ArgumentError.value(id, 'id', 'unknown server');
    }
    if (activeServerId != null && activeServerId != id) {
      onDisconnectActive?.call();
    }
    activeServerId = id;
    final record = servers.firstWhere((s) => s.id == id);
    record.lastUsedAt = DateTime.now().toUtc();
  }

  void setLastLocation(String id, LastLocation location) {
    servers.firstWhere((s) => s.id == id).lastLocation = location;
  }

  void setPushIntent(String id, PushIntent intent) {
    servers.firstWhere((s) => s.id == id).pushIntent = intent;
  }

  Future<void> remove(String id) async {
    if (activeServerId == id) {
      onDisconnectActive?.call();
      activeServerId = null;
    }
    servers.removeWhere((s) => s.id == id);
    await persist();
  }
}

String _newId(Random random) {
  final bytes = List<int>.generate(16, (_) => random.nextInt(256));
  bytes[6] = (bytes[6] & 0x0f) | 0x40;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;
  String hex(int b) => b.toRadixString(16).padLeft(2, '0');
  final h = bytes.map(hex).join();
  return '${h.substring(0, 8)}-${h.substring(8, 12)}-${h.substring(12, 16)}-${h.substring(16, 20)}-${h.substring(20)}';
}
