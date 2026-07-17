/**
 * notification_auth.m
 *
 * Business Logic（为什么需要这个文件）:
 *   macOS 通知权限按应用代码签名身份记账（Dev/Release 不同 Bundle ID 应独立）。
 *   tauri-plugin-notification 桌面 stub 恒返回 Granted，无法反映真实状态。
 *
 * Code Logic（这个文件做什么）:
 *   通过 UNUserNotificationCenter 查询/请求授权；调用方须在非主线程等待。
 *   cargo test 裸二进制无有效 mainBundle 时 UNUserNotificationCenter 会抛异常，
 *   必须 @try/@catch 并 fail-closed 返回 -1，不得崩溃测试进程。
 */

#import "notification_auth.h"

#import <ApplicationServices/ApplicationServices.h>
#import <Foundation/Foundation.h>
#import <UserNotifications/UserNotifications.h>
#import <dispatch/dispatch.h>

int cp_notification_auth_status(void) {
  @try {
    NSBundle *mainBundle = [NSBundle mainBundle];
    if (mainBundle == nil || mainBundle.bundleURL == nil) {
      return -1;
    }
    // 裸 cargo test binary 的 bundleURL 常指向 deps/，无有效 Info.plist / bundle id
    NSString *bid = mainBundle.bundleIdentifier;
    if (bid == nil || bid.length == 0) {
      return -1;
    }

    __block int status = -1;
    dispatch_semaphore_t sem = dispatch_semaphore_create(0);
    UNUserNotificationCenter *center = [UNUserNotificationCenter currentNotificationCenter];
    [center getNotificationSettingsWithCompletionHandler:^(UNNotificationSettings *settings) {
      status = (int)settings.authorizationStatus;
      dispatch_semaphore_signal(sem);
    }];

    long wait = dispatch_semaphore_wait(
        sem, dispatch_time(DISPATCH_TIME_NOW, (int64_t)(3 * NSEC_PER_SEC)));
    if (wait != 0) {
      return -1;
    }
    return status;
  } @catch (NSException *ex) {
    (void)ex;
    return -1;
  }
}

int cp_notification_request_authorization(void) {
  @try {
    NSBundle *mainBundle = [NSBundle mainBundle];
    if (mainBundle == nil || mainBundle.bundleIdentifier == nil ||
        mainBundle.bundleIdentifier.length == 0) {
      return 0;
    }

    __block int granted = 0;
    dispatch_semaphore_t sem = dispatch_semaphore_create(0);
    UNUserNotificationCenter *center = [UNUserNotificationCenter currentNotificationCenter];
    UNAuthorizationOptions options =
        (UNAuthorizationOptionAlert | UNAuthorizationOptionSound | UNAuthorizationOptionBadge);
    [center requestAuthorizationWithOptions:options
                          completionHandler:^(BOOL ok, NSError *_Nullable error) {
                            (void)error;
                            granted = ok ? 1 : 0;
                            dispatch_semaphore_signal(sem);
                          }];
    long wait = dispatch_semaphore_wait(
        sem, dispatch_time(DISPATCH_TIME_NOW, (int64_t)(30 * NSEC_PER_SEC)));
    if (wait != 0) {
      return 0;
    }
    return granted;
  } @catch (NSException *ex) {
    (void)ex;
    return 0;
  }
}

bool cp_request_accessibility_prompt(void) {
  @try {
    // 仅用户手势路径：prompt=YES 会登记列表并可弹出系统辅助功能引导
    NSDictionary *options = @{(__bridge id)kAXTrustedCheckOptionPrompt : @YES};
    return AXIsProcessTrustedWithOptions((__bridge CFDictionaryRef)options);
  } @catch (NSException *ex) {
    (void)ex;
    return AXIsProcessTrusted();
  }
}
