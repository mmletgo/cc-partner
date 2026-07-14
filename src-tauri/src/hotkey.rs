//! hotkey.rs — 快捷键格式转换与可补偿的全局截图快捷键替换
//!
//! Business Logic（为什么需要这个模块）:
//!     config 持久化的是 pynput 格式（如 `<cmd>+<shift>+s`），插件接受 `CommandOrControl+Shift+S`。
//!     热更新若先 `unregister_all` 再注册，OS 注册与 config 落盘之间会留下“无快捷键”或“新旧不一致”窗口。
//!     需要“注册新 → 注销旧 → 持久化 → 失败补偿”的可测试事务。
//!
//! Code Logic（这个模块做什么）:
//!     - 格式转换与 `parse_shortcut`
//!     - `GlobalShortcutBackend` 抽象真实插件与测试 Fake
//!     - `replace_screenshot_hotkey_os` / `compensate_screenshot_hotkey_os`

use crate::error::AppError;
use tauri::AppHandle;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutEvent, ShortcutState};

/// pynput 格式 → tauri-plugin-global-shortcut 格式。
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
                other => other
                    .trim_start_matches('<')
                    .trim_end_matches('>')
                    .to_uppercase(),
            }
        })
        .collect::<Vec<_>>()
        .join("+")
}

/// 把 pynput 格式快捷键解析成插件 `Shortcut`（解析失败返回 None）。
pub fn parse_shortcut(hotkey_pynput: &str) -> Option<Shortcut> {
    let plugin_fmt = hotkey_pynput_to_plugin(hotkey_pynput);
    plugin_fmt.parse::<Shortcut>().ok()
}

/// 已提交的快捷键变更描述（供命令层/测试断言）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredHotkeyChange {
    pub old_value: String,
    pub new_value: String,
    /// OS 侧是否与配置一致（未改或已成功补偿/提交）。
    pub committed: bool,
}

/// 全局快捷键后端抽象（生产走插件，测试走 Fake）。
pub trait GlobalShortcutBackend: Send + Sync {
    fn register_hotkey(&mut self, hotkey_pynput: &str) -> Result<(), AppError>;
    fn unregister_hotkey(&mut self, hotkey_pynput: &str) -> Result<(), AppError>;
    #[allow(dead_code)]
    fn registered(&self) -> Vec<String>;
}

/// 真实 Tauri global-shortcut 后端。
pub struct TauriGlobalShortcutBackend {
    app: AppHandle,
}

impl TauriGlobalShortcutBackend {
    /// 构造生产后端。
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl GlobalShortcutBackend for TauriGlobalShortcutBackend {
    fn register_hotkey(&mut self, hotkey_pynput: &str) -> Result<(), AppError> {
        let Some(shortcut) = parse_shortcut(hotkey_pynput) else {
            return Err(AppError::validation(format!(
                "screenshot_hotkey 无法解析: {hotkey_pynput}"
            )));
        };
        self.app
            .global_shortcut()
            .on_shortcut(shortcut, screenshot_handler)
            .map_err(|e| AppError::generic(format!("注册全局快捷键失败（{hotkey_pynput}）: {e}")))
    }

    fn unregister_hotkey(&mut self, hotkey_pynput: &str) -> Result<(), AppError> {
        let Some(shortcut) = parse_shortcut(hotkey_pynput) else {
            return Err(AppError::validation(format!(
                "screenshot_hotkey 无法解析: {hotkey_pynput}"
            )));
        };
        self.app
            .global_shortcut()
            .unregister(shortcut)
            .map_err(|e| AppError::generic(format!("反注册全局快捷键失败（{hotkey_pynput}）: {e}")))
    }

    fn registered(&self) -> Vec<String> {
        Vec::new()
    }
}

/// 可测试的内存 Fake 快捷键后端。
#[cfg(test)]
#[derive(Debug, Default, Clone)]
pub struct FakeGlobalShortcutBackend {
    pub registered: Vec<String>,
    pub fail_register: Vec<String>,
    pub fail_unregister: Vec<String>,
}

#[cfg(test)]
impl GlobalShortcutBackend for FakeGlobalShortcutBackend {
    fn register_hotkey(&mut self, hotkey_pynput: &str) -> Result<(), AppError> {
        if parse_shortcut(hotkey_pynput).is_none() {
            return Err(AppError::validation(format!(
                "screenshot_hotkey 无法解析: {hotkey_pynput}"
            )));
        }
        if self.fail_register.iter().any(|h| h == hotkey_pynput) {
            return Err(AppError::generic(format!("注入: 注册失败 {hotkey_pynput}")));
        }
        if self.registered.iter().any(|h| h == hotkey_pynput) {
            return Err(AppError::conflict(format!("快捷键已注册: {hotkey_pynput}")));
        }
        self.registered.push(hotkey_pynput.to_string());
        Ok(())
    }

    fn unregister_hotkey(&mut self, hotkey_pynput: &str) -> Result<(), AppError> {
        if self.fail_unregister.iter().any(|h| h == hotkey_pynput) {
            return Err(AppError::generic(format!("注入: 注销失败 {hotkey_pynput}")));
        }
        let before = self.registered.len();
        self.registered.retain(|h| h != hotkey_pynput);
        if self.registered.len() == before {
            return Err(AppError::generic(format!("快捷键未注册: {hotkey_pynput}")));
        }
        Ok(())
    }

    fn registered(&self) -> Vec<String> {
        self.registered.clone()
    }
}

/// 在 OS 侧把截图快捷键从 old 切到 new（尚未持久化）。
///
/// Business Logic（为什么需要这个函数）:
///     热更新必须先确保新键可用，再去掉旧键。
///
/// Code Logic（这个函数做什么）:
///     相等 → committed true；否则 register new → unregister old；失败补偿。
pub fn replace_screenshot_hotkey_os(
    backend: &mut dyn GlobalShortcutBackend,
    old_value: &str,
    new_value: &str,
) -> Result<RegisteredHotkeyChange, AppError> {
    if old_value == new_value {
        return Ok(RegisteredHotkeyChange {
            old_value: old_value.to_string(),
            new_value: new_value.to_string(),
            committed: true,
        });
    }
    if parse_shortcut(new_value).is_none() {
        return Err(AppError::validation(format!(
            "screenshot_hotkey 无法解析: {new_value}"
        )));
    }
    backend.register_hotkey(new_value)?;
    if parse_shortcut(old_value).is_some() {
        if let Err(e) = backend.unregister_hotkey(old_value) {
            let _ = backend.unregister_hotkey(new_value);
            return Err(e);
        }
    }
    Ok(RegisteredHotkeyChange {
        old_value: old_value.to_string(),
        new_value: new_value.to_string(),
        committed: false,
    })
}

/// 持久化失败后，把 OS 注册从 new 补偿回 old。
///
/// Business Logic（为什么需要这个函数）:
///     config 事务失败时磁盘/内存保持旧值，OS 也必须回到旧值。
///
/// Code Logic（这个函数做什么）:
///     re-register old → unregister new；失败返回 hotkey.rollback_failed。
pub fn compensate_screenshot_hotkey_os(
    backend: &mut dyn GlobalShortcutBackend,
    old_value: &str,
    new_value: &str,
) -> Result<(), AppError> {
    if old_value == new_value {
        return Ok(());
    }
    if let Err(e) = backend.register_hotkey(old_value) {
        return Err(AppError::generic(format!(
            "hotkey.rollback_failed: 恢复旧快捷键失败（请重启应用）: {e}"
        )));
    }
    if let Err(e) = backend.unregister_hotkey(new_value) {
        return Err(AppError::generic(format!(
            "hotkey.rollback_failed: 移除新快捷键失败（请重启应用）: {e}"
        )));
    }
    Ok(())
}

/// 注册截图快捷键（启动路径）。
///
/// Business Logic: 应用启动时注册配置中的截图快捷键。
/// Code Logic: 先尝试注销同 shortcut，再 on_shortcut；热更新请用 replace 路径。
pub fn register_screenshot_hotkey<F>(app: &AppHandle, hotkey_pynput: &str, handler: F) -> bool
where
    F: Fn(&AppHandle, &Shortcut, ShortcutEvent) + Send + Sync + 'static,
{
    let Some(shortcut) = parse_shortcut(hotkey_pynput) else {
        tracing::error!("无法解析截图快捷键（pynput={}），跳过注册", hotkey_pynput);
        return false;
    };
    let _ = app.global_shortcut().unregister(shortcut);
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

    #[test]
    fn replace_parse_failure_leaves_old() {
        let mut fake = FakeGlobalShortcutBackend {
            registered: vec!["<ctrl>+s".into()],
            ..Default::default()
        };
        let err = replace_screenshot_hotkey_os(&mut fake, "<ctrl>+s", "not-a-hotkey")
            .expect_err("parse fail");
        assert!(err.to_string().contains("无法解析"));
        assert_eq!(fake.registered(), vec!["<ctrl>+s".to_string()]);
    }

    #[test]
    fn replace_new_register_conflict_leaves_old() {
        let mut fake = FakeGlobalShortcutBackend {
            registered: vec!["<ctrl>+s".into()],
            fail_register: vec!["<ctrl>+<shift>+s".into()],
            ..Default::default()
        };
        let err =
            replace_screenshot_hotkey_os(&mut fake, "<ctrl>+s", "<ctrl>+<shift>+s").unwrap_err();
        assert!(err.to_string().contains("注册失败"));
        assert_eq!(fake.registered(), vec!["<ctrl>+s".to_string()]);
    }

    #[test]
    fn replace_old_unregister_failure_rolls_back_new() {
        let mut fake = FakeGlobalShortcutBackend {
            registered: vec!["<ctrl>+s".into()],
            fail_unregister: vec!["<ctrl>+s".into()],
            ..Default::default()
        };
        let err =
            replace_screenshot_hotkey_os(&mut fake, "<ctrl>+s", "<ctrl>+<shift>+s").unwrap_err();
        assert!(err.to_string().contains("注销失败"));
        assert!(fake.registered().contains(&"<ctrl>+s".to_string()));
        assert!(!fake.registered().contains(&"<ctrl>+<shift>+s".to_string()));
    }

    #[test]
    fn replace_success_registers_new_unregisters_old() {
        let mut fake = FakeGlobalShortcutBackend {
            registered: vec!["<ctrl>+s".into()],
            ..Default::default()
        };
        let change =
            replace_screenshot_hotkey_os(&mut fake, "<ctrl>+s", "<ctrl>+<shift>+s").unwrap();
        assert!(!change.committed);
        assert_eq!(fake.registered(), vec!["<ctrl>+<shift>+s".to_string()]);
    }

    #[test]
    fn replace_unchanged_is_noop_success() {
        let mut fake = FakeGlobalShortcutBackend {
            registered: vec!["<ctrl>+s".into()],
            ..Default::default()
        };
        let change = replace_screenshot_hotkey_os(&mut fake, "<ctrl>+s", "<ctrl>+s").unwrap();
        assert!(change.committed);
        assert_eq!(fake.registered(), vec!["<ctrl>+s".to_string()]);
    }

    #[test]
    fn compensate_success_restores_old() {
        let mut fake = FakeGlobalShortcutBackend {
            registered: vec!["<ctrl>+<shift>+s".into()],
            ..Default::default()
        };
        compensate_screenshot_hotkey_os(&mut fake, "<ctrl>+s", "<ctrl>+<shift>+s").unwrap();
        assert_eq!(fake.registered(), vec!["<ctrl>+s".to_string()]);
    }

    #[test]
    fn compensate_failure_returns_rollback_failed() {
        let mut fake = FakeGlobalShortcutBackend {
            registered: vec!["<ctrl>+<shift>+s".into()],
            fail_register: vec!["<ctrl>+s".into()],
            ..Default::default()
        };
        let err =
            compensate_screenshot_hotkey_os(&mut fake, "<ctrl>+s", "<ctrl>+<shift>+s").unwrap_err();
        assert!(err.to_string().contains("hotkey.rollback_failed"));
        assert_eq!(fake.registered(), vec!["<ctrl>+<shift>+s".to_string()]);
    }
}
