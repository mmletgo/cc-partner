/**
 * input_monitoring_auth.h
 *
 * Business Logic（为什么需要这个文件）:
 *   macOS 输入监控（ListenEvent）列表登记依赖系统 Request API；
 *   无 NSApplication/RunLoop 时弹窗常不出现，Request 立即返回 false。
 *
 * Code Logic（这个文件做什么）:
 *   声明 `cp_request_listen_event_access`：确保 NSApp + 主线程 Request + 泵 RunLoop。
 */
#pragma once

#ifdef __cplusplus
extern "C" {
#endif

/**
 * Business Logic: Welcome「去设置」需系统弹窗把本 app 写入「输入监控」列表。
 * Code Logic: 主线程确保 NSApplication → IOHIDRequestAccess + CGRequestListenEventAccess →
 *     短暂泵 NSRunLoop（给模态弹窗时间）；任一侧 true 或最终 IOHID Granted 返回 1，否则 0。
 */
int cp_request_listen_event_access(void);

#ifdef __cplusplus
}
#endif
