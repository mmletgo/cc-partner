import 'address_book/book.dart';
import 'settings/risk_copy.dart';

/// Native entry. Flutter widgets are layered on this package; the workbench is
/// not a WebView of `/mobile`.
void main() {
  final book = AddressBook(store: MemoryAddressBookStore());
  // ignore: avoid_print
  print('cc-partner mobile; servers=${book.servers.length}; risk=$kLanRiskStatement');
}
