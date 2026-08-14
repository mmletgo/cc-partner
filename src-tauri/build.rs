use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// Tauri 构建脚本入口。
///
/// Business Logic（为什么需要这个函数）:
///     应用图标会影响 Dock、托盘和三平台安装包；用户更换图标后需要 Rust 侧重新生成
///     Tauri context，否则运行中的默认窗口图标可能继续使用旧的嵌入资源。debug/cargo-check 时
///     Tauri externalBin 也需要一个目标 triple 命名的 sidecar 文件才能通过构建期校验。
///
/// Code Logic（这个函数做什么）:
///     显式声明 tauri 配置和图标文件为 Cargo build script 依赖；debug profile 下生成开发期 sidecar
///     launcher；确保 `resources/browser-runtime` 可被 Tauri resource glob 匹配；最后委托
///     `tauri_build::build()` 生成 Tauri 所需的编译期上下文。
fn main() {
    println!("cargo:rerun-if-changed=tauri.conf.json");
    println!("cargo:rerun-if-changed=tauri.internal.conf.json");
    println!("cargo:rerun-if-changed=icons/32x32.png");
    println!("cargo:rerun-if-changed=icons/128x128.png");
    println!("cargo:rerun-if-changed=icons/128x128@2x.png");
    println!("cargo:rerun-if-changed=icons/icon.icns");
    println!("cargo:rerun-if-changed=icons/icon.ico");
    println!("cargo:rerun-if-changed=icons/tray-icon.png");
    println!("cargo:rerun-if-changed=native/macos/notification_auth.m");
    println!("cargo:rerun-if-changed=native/macos/notification_auth.h");
    println!("cargo:rerun-if-env-changed=PROFILE");
    println!("cargo:rerun-if-env-changed=TARGET");
    ensure_debug_sidecar_launcher();
    ensure_browser_runtime_resource();
    compile_macos_notification_auth();
    tauri_build::build()
}

/// 保证 Tauri `bundle.resources` 的 `resources/browser-runtime/**/*` 至少能匹配一个文件。
///
/// Business Logic（为什么需要这个函数）:
///     managed Chromium 目录被 gitignore，干净克隆或误提交的自指/失效 symlink 会让
///     `tauri_build` 因 glob 未匹配直接失败，阻断 `cargo check` / `tauri dev` / clippy。
///
/// Code Logic（这个函数做什么）:
///     若路径是损坏/自指 symlink 或普通文件则删除后重建目录；目录为空时写入
///     `.platform-unavailable` 占位。已有真实 runtime 内容则不动。
fn ensure_browser_runtime_resource() {
    let runtime_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("resources")
        .join("browser-runtime");
    println!("cargo:rerun-if-changed=resources/browser-runtime");

    let usable = browser_runtime_dir_has_entries(&runtime_root);
    if !usable {
        replace_unusable_browser_runtime_path(&runtime_root);
        if let Err(error) = fs::create_dir_all(&runtime_root) {
            panic!("创建 browser-runtime 资源目录失败: {error}");
        }
    }

    if browser_runtime_dir_has_entries(&runtime_root) {
        return;
    }

    let placeholder = runtime_root.join(".platform-unavailable");
    if let Err(error) = fs::write(
        &placeholder,
        "managed browser runtime not prepared; run node scripts/prepare-browser-runtime.mjs --platform current\n",
    ) {
        panic!("写入 browser-runtime 占位失败: {error}");
    }
}

/// 判断 browser-runtime 路径是否已是可读且非空的目录。
///
/// Code Logic（这个函数做什么）:
///     `read_dir` 会跟随 symlink；自指/损坏链接或空目录返回 false。
fn browser_runtime_dir_has_entries(path: &Path) -> bool {
    match fs::read_dir(path) {
        Ok(entries) => entries.filter_map(Result::ok).next().is_some(),
        Err(_) => false,
    }
}

/// 删除无法作为资源根的 browser-runtime 路径（损坏 symlink 或普通文件）。
///
/// Code Logic（这个函数做什么）:
///     只对 symlink / 普通文件 `remove_file`，避免 `remove_dir_all` 误删 symlink 指向的目标。
fn replace_unusable_browser_runtime_path(path: &Path) {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => return,
    };
    let file_type = metadata.file_type();
    if file_type.is_symlink() || file_type.is_file() {
        if let Err(error) = fs::remove_file(path) {
            panic!("移除无效 browser-runtime 路径失败: {error}");
        }
    }
}

/// 编译 macOS 通知权限 ObjC 桥接。
///
/// Business Logic（为什么需要这个函数）:
///   Dev/Release 需独立通知授权状态；plugin 桌面 stub 恒 Granted，需原生 UNUserNotificationCenter。
///
/// Code Logic（这个函数做什么）:
///   仅 darwin target 用 `cc` 编译 `notification_auth.m` 并链接 UserNotifications/Foundation。
fn compile_macos_notification_auth() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }

    let manifest_dir = PathBuf::from(
        env::var("CARGO_MANIFEST_DIR")
            .unwrap_or_else(|_| panic!("编译 notification_auth 时缺少 CARGO_MANIFEST_DIR")),
    );
    let src = manifest_dir.join("native/macos/notification_auth.m");
    if !src.exists() {
        panic!("missing {}", src.display());
    }

    cc::Build::new()
        .file(&src)
        .include(manifest_dir.join("native/macos"))
        .flag("-fobjc-arc")
        .compile("cp_notification_auth");

    println!("cargo:rustc-link-lib=framework=UserNotifications");
    println!("cargo:rustc-link-lib=framework=Foundation");
    println!("cargo:rustc-link-lib=framework=ApplicationServices");
}

/// 生成 debug profile 使用的 sidecar launcher。
///
/// Business Logic（为什么需要这个函数）:
///     `cargo check` 和 `tauri dev` 不会预先准备 release sidecar 二进制，但 Tauri externalBin 会在构建期校验
///     `binaries/cc-partner-backend-<target>` 是否存在；缺文件会阻断开发验证。
///
/// Code Logic（这个函数做什么）:
///     仅在 PROFILE=debug 时创建当前 TARGET 对应的 launcher 文件；release profile 不生成占位，确保正式打包仍使用真实二进制。
fn ensure_debug_sidecar_launcher() {
    if env::var("PROFILE").as_deref() != Ok("debug") {
        return;
    }

    let Ok(target) = env::var("TARGET") else {
        return;
    };
    let launcher_path = sidecar_launcher_path(&target);
    if launcher_path.exists() {
        return;
    }

    if let Some(parent) = launcher_path.parent() {
        if let Err(error) = fs::create_dir_all(parent) {
            panic!("创建 debug sidecar 目录失败: {error}");
        }
    }

    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| {
        panic!("构建 backend sidecar launcher 时缺少 CARGO_MANIFEST_DIR 环境变量")
    });
    let content = debug_sidecar_launcher_content(&target, &manifest_dir);
    if let Err(error) = fs::write(&launcher_path, content) {
        panic!("写入 debug sidecar launcher 失败: {error}");
    }
    make_executable(&launcher_path);
}

/// 返回当前 target 对应的 sidecar launcher 路径。
///
/// Business Logic（为什么需要这个函数）:
///     Tauri externalBin 会按目标 triple 查找 sidecar 文件，不同平台需要稳定的文件名生成规则。
///
/// Code Logic（这个函数做什么）:
///     基于 CARGO_MANIFEST_DIR 拼出 `binaries/cc-partner-backend-<target>`；Windows target 追加 `.exe`。
fn sidecar_launcher_path(target: &str) -> PathBuf {
    let mut filename = format!("cc-partner-backend-{target}");
    if target.contains("windows") {
        filename.push_str(".exe");
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("binaries")
        .join(filename)
}

/// 返回 debug sidecar launcher 文件内容。
///
/// Business Logic（为什么需要这个函数）:
///     开发期 Tauri externalBin 构建期校验需要 `binaries/cc-partner-backend-<target>` 存在；
///     该 launcher 在 GUI 通过 Tauri sidecar 机制调用时真正拉起 backend。
///
/// Code Logic（这个函数做什么）:
///     Unix target 返回可执行 shell launcher；Windows target 写入说明性占位（Windows dev 走 dev binary fallback）。
///     launcher 必须只通过 `cargo run --bin cc-partner-backend` 启动，**不能 exec 任何 target/debug 真二进制**。
///     原因：Tauri externalBin 在 dev 模式会把 `binaries/cc-partner-backend-<target>` 复制到
///     `target/debug/cc-partner-backend` 覆盖 cargo 原始产物。如果 launcher 里 `exec target/debug/cc-partner-backend`，
///     而 launcher 自身已被复制到那个路径，就会无限自递归。直接 `cargo run` 不经过任何会被覆盖的路径，
///     且 cargo 检测到已编译会直接运行产物，首次外的开销极低。manifest_dir 在构建期烧入，运行位置无关。
fn debug_sidecar_launcher_content(target: &str, manifest_dir: &str) -> String {
    if target.contains("windows") {
        return "debug placeholder for cc-partner-backend sidecar\n".to_string();
    }
    format!(
        r#"#!/usr/bin/env sh
set -eu
MANIFEST_DIR={manifest_dir:?}
exec cargo run --manifest-path "$MANIFEST_DIR/Cargo.toml" --bin cc-partner-backend -- "$@"
"#
    )
}

/// 在 Unix 平台给 launcher 增加执行权限。
///
/// Business Logic（为什么需要这个函数）:
///     Tauri shell sidecar 需要直接执行 launcher；Unix 文件缺少执行位会导致 dev fallback 前先报权限错误。
///
/// Code Logic（这个函数做什么）:
///     Unix 下把文件权限设为 755；非 Unix 平台为空操作。
fn make_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path)
            .unwrap_or_else(|error| panic!("读取 debug sidecar 权限失败: {error}"))
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)
            .unwrap_or_else(|error| panic!("设置 debug sidecar 执行权限失败: {error}"));
    }

    #[cfg(not(unix))]
    {
        let _ = path;
    }
}
