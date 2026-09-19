# mobile/ — Flutter / Dart 原生客户端

局域网工作台客户端。业务逻辑在 `lib/`，Flutter 壳在 `lib/app.dart` 与 `lib/ui/`。不要修改 `web/src/mobile/`。

```bash
export PATH="$HOME/flutter/bin:$PATH"
export DEVELOPER_DIR=/Applications/Xcode-beta.app/Contents/Developer
export JAVA_HOME="/Applications/Android Studio.app/Contents/jbr/Contents/Home"
export ANDROID_HOME="$HOME/Library/Android/sdk"

cd mobile
flutter test
flutter test integration_test/app_test.dart -d emulator-5554
flutter test integration_test/app_test.dart -d <ios-simulator-id>
flutter run -d emulator-5554
flutter run -d <ios-simulator-id>
```
