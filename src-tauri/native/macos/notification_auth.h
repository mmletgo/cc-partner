#pragma once

#include <stdbool.h>

#ifdef __cplusplus
extern "C" {
#endif

/**
 * Business Logic:
 *   Dev/Release 使用不同 Bundle ID，通知权限应按 app 身份独立；
 *   tauri-plugin-notification 桌面端恒返回 Granted，不能作为权威。
 *
 * Code Logic:
 *   查询 UNUserNotificationCenter 的 authorizationStatus。
 *
 * @return 0=notDetermined, 1=denied, 2=authorized, 3=provisional, 4=ephemeral, -1=error
 */
int cp_notification_auth_status(void);

/**
 * Business Logic:
 *   Welcome「去设置」需要触发系统通知授权对话框（仅 notDetermined 时弹框）。
 *
 * Code Logic:
 *   requestAuthorizationWithOptions(alert|sound|badge)，同步等待 completion。
 *
 * @return 1=用户授权(含 provisional/ephemeral), 0=拒绝或失败
 */
int cp_notification_request_authorization(void);

/**
 * Business Logic:
 *   用户点击 Welcome「去设置」时把 app 登记进「隐私 → 辅助功能」列表；
 *   禁止在权限 check / app 启动路径调用（会弹系统框）。
 *
 * Code Logic:
 *   AXIsProcessTrustedWithOptions({kAXTrustedCheckOptionPrompt: YES})。
 *
 * @return 当前是否已信任
 */
bool cp_request_accessibility_prompt(void);

#ifdef __cplusplus
}
#endif
