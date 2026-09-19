import Flutter
import UIKit
import UserNotifications

@main
@objc class AppDelegate: FlutterAppDelegate, FlutterImplicitEngineDelegate, NetServiceBrowserDelegate {
  private static var pendingToken: String?
  private static var channel: FlutterMethodChannel?
  /// 短时 Bonjour browse，只为弹出 iOS 本地网络授权，不消费发现结果。
  private var localNetworkBrowser: NetServiceBrowser?

  override func application(
    _ application: UIApplication,
    didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?
  ) -> Bool {
    triggerLocalNetworkPermission()
    UNUserNotificationCenter.current().delegate = self
    UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .badge, .sound]) { _, _ in
      DispatchQueue.main.async {
        application.registerForRemoteNotifications()
      }
    }
    return super.application(application, didFinishLaunchingWithOptions: launchOptions)
  }

  /// 用一次 `_cc-partner._tcp` 浏览触发系统本地网络授权框。
  ///
  /// 第一版不做手机 mDNS 发现；结果丢弃。没有这次局域网操作时，
  /// dart:io 访问 192.168.x.x 会在无提示的情况下失败。
  private func triggerLocalNetworkPermission() {
    let browser = NetServiceBrowser()
    browser.delegate = self
    localNetworkBrowser = browser
    browser.searchForServices(ofType: "_cc-partner._tcp.", inDomain: "local.")
    DispatchQueue.main.asyncAfter(deadline: .now() + 3) { [weak self] in
      self?.localNetworkBrowser?.stop()
      self?.localNetworkBrowser = nil
    }
  }

  func netServiceBrowser(_ browser: NetServiceBrowser, didFind service: NetService, moreComing: Bool) {}

  func netServiceBrowser(_ browser: NetServiceBrowser, didNotSearch errorDict: [String : NSNumber]) {}

  override func application(
    _ application: UIApplication,
    didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data
  ) {
    let token = deviceToken.map { String(format: "%02x", $0) }.joined()
    AppDelegate.pendingToken = token
    AppDelegate.channel?.invokeMethod("onToken", arguments: token)
  }

  func didInitializeImplicitFlutterEngine(_ engineBridge: FlutterImplicitEngineBridge) {
    GeneratedPluginRegistrant.register(with: engineBridge.pluginRegistry)
    let channel = FlutterMethodChannel(
      name: "cc_partner/push",
      binaryMessenger: engineBridge.applicationRegistrar.messenger()
    )
    channel.setMethodCallHandler { call, result in
      if call.method == "getToken" {
        result(AppDelegate.pendingToken)
      } else {
        result(FlutterMethodNotImplemented)
      }
    }
    AppDelegate.channel = channel
  }
}
