//! hotkey.rs — 快捷键格式转换
//!
//! Business Logic（为什么需要这个模块）:
//!     config 持久化的是 pynput 格式（如 `<cmd>+<shift>+s`，对照 Python `hotkey/listener.py`），
//!     而 `tauri-plugin-global-shortcut` 接受的格式是 `CommandOrControl+Shift+S`。
//!     注册/热更新快捷键前需做一次转换。
//!
//! Code Logic（这个模块做什么）:
//!     `hotkey_pynput_to_plugin` 按 `+` 拆分，逐段映射修饰键（`<cmd>`/`<ctrl>` → CommandOrControl、
//!     `<shift>` → Shift、`<alt>` → Alt / Option），普通字母键转大写，最后用 `+` 连接。

/// pynput 格式 → tauri-plugin-global-shortcut 格式。
///
/// 示例：`<cmd>+<shift>+s` → `CommandOrControl+Shift+S`；
///       `<ctrl>+<shift>+s` → `CommandOrControl+Shift+S`（Ctrl/Cmd 统一映射为 CommandOrControl，
///       由插件按平台解析，macOS=Command、Windows/Linux=Ctrl）。
pub fn hotkey_pynput_to_plugin(hotkey: &str) -> String {
    hotkey
        .split('+')
        .map(|part| {
            let p = part.trim().to_ascii_lowercase();
            match p.as_str() {
                "<cmd>" | "<cmd_r>" | "<cmd_l>" | "<win>" | "<ctrl>" | "<ctrl_l>" | "<ctrl_r>" => {
                    "CommandOrControl".to_string()
                }
                "<shift>" | "<shift_l>" | "<shift_r>" => "Shift".to_string(),
                "<alt>" | "<alt_l>" | "<alt_r>" | "<option>" => "Option".to_string(),
                // 修饰键之外的普通键：去掉尖括号后大写（如 "s" → "S"）
                other => other
                    .trim_start_matches('<')
                    .trim_end_matches('>')
                    .to_uppercase(),
            }
        })
        .collect::<Vec<_>>()
        .join("+")
}

// ── 全局快捷键注册/热更新（封装 tauri-plugin-global-shortcut v2 API）───────
use tauri::AppHandle;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutEvent, ShortcutState};

/// 把 pynput 格式快捷键解析成插件 `Shortcut`（解析失败返回 None）。
///
/// Business Logic: setup 与 update_config 都需把 config 里的字符串转成可注册的 Shortcut。
pub fn parse_shortcut(hotkey_pynput: &str) -> Option<Shortcut> {
    let plugin_fmt = hotkey_pynput_to_plugin(hotkey_pynput);
    plugin_fmt.parse::<Shortcut>().ok()
}

/// 反注册全部全局快捷键。
///
/// Business Logic: update_config 改快捷键时先清掉旧的，避免重复触发。
/// Code Logic: `unregister_all` 一把清空（M7 桌面端只有一个截图快捷键，简单可靠）。
pub fn unregister_all(app: &AppHandle) {
    if let Err(e) = app.global_shortcut().unregister_all() {
        tracing::warn!("反注册全局快捷键失败: {e}");
    }
}

/// 注册截图快捷键（先反注册全部，再带 handler 注册新的）。
///
/// Business Logic: 应用启动 + 用户改快捷键后调用。v2 的 `on_shortcut` 需随快捷键传入 handler，
///     故 handler 由调用方提供（lib.rs / commands/config.rs 各传一份相同闭包，触发截图 overlay）。
/// Code Logic: parse 失败则记日志并跳过（不阻断应用）。返回是否注册成功。
pub fn register_screenshot_hotkey<F>(app: &AppHandle, hotkey_pynput: &str, handler: F) -> bool
where
    F: Fn(&AppHandle, &Shortcut, ShortcutEvent) + Send + Sync + 'static,
{
    unregister_all(app);
    let Some(shortcut) = parse_shortcut(hotkey_pynput) else {
        tracing::error!("无法解析截图快捷键（pynput={}），跳过注册", hotkey_pynput);
        return false;
    };
    match app.global_shortcut().on_shortcut(shortcut, handler) {
        Ok(()) => {
            tracing::info!("已注册截图快捷键: {}", hotkey_pynput);
            true
        }
        Err(e) => {
            tracing::error!("注册全局快捷键失败（{}）: {e}", hotkey_pynput);
            false
        }
    }
}

/// 截图快捷键 handler：按下时触发 `start_region_capture`。
///
/// Business Logic: 统一构造 handler，避免 lib.rs setup 与 commands::config::update_config 重复闭包。
pub fn screenshot_handler(app: &AppHandle, _shortcut: &Shortcut, event: ShortcutEvent) {
    if event.state == ShortcutState::Pressed {
        if let Err(e) = crate::screenshot::overlay::start_region_capture(app) {
            tracing::error!("快捷键触发截图失败: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_macos_cmd_shift_s() {
        assert_eq!(
            hotkey_pynput_to_plugin("<cmd>+<shift>+s"),
            "CommandOrControl+Shift+S"
        );
    }

    #[test]
    fn converts_cross_platform_ctrl() {
        assert_eq!(
            hotkey_pynput_to_plugin("<ctrl>+<shift>+s"),
            "CommandOrControl+Shift+S"
        );
    }

    #[test]
    fn converts_alt_variant() {
        assert_eq!(
            hotkey_pynput_to_plugin("<ctrl>+<alt>+s"),
            "CommandOrControl+Option+S"
        );
    }
}
