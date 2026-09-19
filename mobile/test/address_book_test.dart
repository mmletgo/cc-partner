import 'package:cc_partner_mobile/address_book/book.dart';
import 'package:cc_partner_mobile/address_book/models.dart';
import 'package:test/test.dart';

HealthSnapshot _online(String id) => HealthSnapshot(
  ok: true,
  deviceId: id,
  deviceName: 'PC-$id',
  protocolVersion: 1,
  capabilities: const ['attention.v2'],
);

void main() {
  test(
    'dedupes normalized host:port instead of inserting a second row',
    () async {
      final book = AddressBook(store: MemoryAddressBookStore());
      await book.addFromInput(
        'http://192.168.1.8:62116/mobile',
        probe: (_) async => _online('pc-a'),
      );
      await book.addFromInput(
        'HTTP://192.168.1.8:62116/mobile/foo',
        probe: (_) async => _online('pc-a'),
      );
      expect(book.servers, hasLength(1));
      expect(book.servers.single.baseUrl, 'http://192.168.1.8:62116');
    },
  );

  test('force-save of unreachable host is never marked online', () async {
    final book = AddressBook(store: MemoryAddressBookStore());
    final record = await book.addFromInput(
      '10.0.0.9:62116',
      probe: (_) async => throw Exception('timeout'),
      forceIfUnreachable: true,
    );
    expect(record.lastHealth, ServerHealth.unreachable);
    expect(record.isOnline, isFalse);
    expect(record.lastProbeError, isNotNull);
    expect(record.lastProbeError, contains('timeout'));
  });

  test('refreshHealth marks a previously unreachable PC online', () async {
    final store = MemoryAddressBookStore();
    final book = AddressBook(store: store, mobileDeviceId: 'phone-1');
    await book.addFromInput(
      '10.0.0.9:62116',
      probe: (_) async => throw Exception('timeout'),
      forceIfUnreachable: true,
    );
    expect(book.servers.single.isOnline, isFalse);

    await book.refreshHealth((_) async => _online('pc-9'));
    expect(book.servers.single.isOnline, isTrue);
    expect(book.servers.single.deviceName, 'PC-pc-9');
    expect(book.servers.single.lastProbeError, isNull);

    final restored = AddressBook(store: store, mobileDeviceId: 'phone-1');
    await restored.load();
    expect(restored.servers.single.isOnline, isTrue);
  });

  test(
    'refreshHealth marks an online PC unreachable when probe fails',
    () async {
      final book = AddressBook(store: MemoryAddressBookStore());
      await book.addFromInput(
        '10.0.0.8:62116',
        probe: (_) async => _online('pc-8'),
      );
      await book.refreshHealth((_) async => throw Exception('down'));
      expect(book.servers.single.isOnline, isFalse);
      expect(book.servers.single.lastProbeError, isNotNull);
    },
  );

  test('unreachable without force does not save', () async {
    final book = AddressBook(store: MemoryAddressBookStore());
    await expectLater(
      book.addFromInput(
        '10.0.0.9',
        probe: (_) async => throw Exception('timeout'),
      ),
      throwsA(isA<ServerUnreachableException>()),
    );
    expect(book.servers, isEmpty);
  });

  test(
    'switch tears active connection without clearing other push intent or lastLocation',
    () async {
      final book = AddressBook(store: MemoryAddressBookStore());
      final a = await book.addFromInput(
        '10.0.0.1',
        probe: (_) async => _online('a'),
      );
      final b = await book.addFromInput(
        '10.0.0.2',
        probe: (_) async => _online('b'),
      );
      book.setLastLocation(
        a.id,
        const LastLocation(projectId: 'p1', panel: 'terminal'),
      );
      book.setPushIntent(a.id, const PushIntent(registeredToken: 'tok-a'));
      book.setPushIntent(b.id, const PushIntent(registeredToken: 'tok-b'));
      book.switchActive(a.id);

      var disconnected = 0;
      book.onDisconnectActive = () => disconnected += 1;
      book.switchActive(b.id);

      expect(disconnected, 1);
      expect(book.activeServerId, b.id);
      expect(
        book.servers.firstWhere((s) => s.id == a.id).lastLocation?.projectId,
        'p1',
      );
      expect(
        book.servers
            .firstWhere((s) => s.id == a.id)
            .pushIntent
            ?.registeredToken,
        'tok-a',
      );
      expect(
        book.servers
            .firstWhere((s) => s.id == b.id)
            .pushIntent
            ?.registeredToken,
        'tok-b',
      );
    },
  );

  test('lastLocation round-trips through the JSON store', () async {
    final store = MemoryAddressBookStore();
    final book = AddressBook(store: store, mobileDeviceId: 'phone-1');
    final record = await book.addFromInput(
      '10.0.0.3:62116',
      probe: (_) async => _online('c'),
    );
    book.setLastLocation(
      record.id,
      const LastLocation(
        projectId: 'proj',
        panel: 'files',
        worktreeId: 'wt',
        sessionId: 'sess',
      ),
    );
    await book.persist();

    final restored = AddressBook(store: store, mobileDeviceId: 'phone-1');
    await restored.load();
    expect(restored.servers, hasLength(1));
    expect(
      restored.servers.single.lastLocation,
      const LastLocation(
        projectId: 'proj',
        panel: 'files',
        worktreeId: 'wt',
        sessionId: 'sess',
      ),
    );
  });
}
