import '../address_book/book.dart';
import '../address_book/models.dart';
import '../core/lan_http.dart';
import 'payload.dart';

const kMobilePushCapability = 'mobile.push.v1';

/// Register the current phone token on every reachable saved PC that advertises the capability.
class PushFanout {
  const PushFanout(this._http);

  final LanHttpClient _http;

  Future<Map<String, String>> registerAll({
    required AddressBook book,
    required String token,
    required String platform,
    required String appBuild,
  }) async {
    final errors = <String, String>{};
    final body = PushRegistration(
      mobileDeviceId: book.mobileDeviceId,
      platform: platform,
      token: token,
      appBuild: appBuild,
    ).toJson();
    for (final server in book.servers) {
      if (server.lastHealth != ServerHealth.online) {
        continue;
      }
      if (!server.capabilities.contains(kMobilePushCapability)) {
        continue;
      }
      try {
        await _http.postJson(server.baseUrl, '/api/mobile/push/register', body);
        book.setPushIntent(server.id, PushIntent(registeredToken: token));
      } catch (error) {
        errors[server.id] = error.toString();
        book.setPushIntent(server.id, PushIntent(lastError: error.toString()));
      }
    }
    return errors;
  }
}
