/**
 * input_monitoring_auth.m
 *
 * Business Logic（为什么需要这个文件）:
 *   输入监控系统弹窗必须在有 NSApplication 的上下文中请求；
 *   纯 Rust FFI 在无 NSApp 的 oneshot 子进程里常「无弹窗 + 立即 false」。
 *   用户点系统弹窗允许后，本 app 才会出现在「隐私 → 输入监控」列表。
 *
 * Code Logic（这个文件做什么）:
 *   - 确保 [NSApplication sharedApplication] 并 Accessory 激活策略；
 *   - 调 IOHIDRequestAccess(ListenEvent) + CGRequestListenEventAccess；
 *   - 泵 NSRunLoop 数秒，让可能出现的模态框有机会展示/完成；
 *   - 返回 1 当 Request 任一侧 true，或泵完后 IOHIDCheckAccess==Granted。
 */

#import "input_monitoring_auth.h"

#import <AppKit/AppKit.h>
#import <CoreGraphics/CoreGraphics.h>
#import <IOKit/hidsystem/IOHIDLib.h>

int cp_request_listen_event_access(void) {
  @try {
    @autoreleasepool {
      NSApplication *app = [NSApplication sharedApplication];
      if (app == nil) {
        return 0;
      }
      // Regular：输入监控系统框在前台激活策略下更易弹出；不自动重启进程。
      [app setActivationPolicy:NSApplicationActivationPolicyRegular];
      [app activateIgnoringOtherApps:YES];

      // 先泵一帧，确保 runloop/窗口服务器就绪
      [[NSRunLoop currentRunLoop]
          runUntilDate:[NSDate dateWithTimeIntervalSinceNow:0.15]];

      // Request 若弹出模态框会阻塞到用户点完；若立即 false 则不要空转几秒（主线程会卡住）。
      // 先 CG 再 IOHID：部分系统上 CG 更易触发中转窗。
      BOOL cg_ok = CGRequestListenEventAccess() ? YES : NO;
      BOOL iohid_ok =
          IOHIDRequestAccess(kIOHIDRequestTypeListenEvent) ? YES : NO;

      IOHIDAccessType access = IOHIDCheckAccess(kIOHIDRequestTypeListenEvent);
      if (iohid_ok || cg_ok || access == kIOHIDAccessTypeGranted) {
        // 同意后短暂泵一下，便于 TCC 落盘
        [[NSRunLoop currentRunLoop]
            runUntilDate:[NSDate dateWithTimeIntervalSinceNow:0.2]];
        return 1;
      }
      return 0;
    }
  } @catch (NSException *ex) {
    (void)ex;
    return 0;
  }
}
