/// Tauri 构建脚本入口。
///
/// Business Logic（为什么需要这个函数）:
///     应用图标会影响 Dock、托盘和三平台安装包；用户更换图标后需要 Rust 侧重新生成
///     Tauri context，否则运行中的默认窗口图标可能继续使用旧的嵌入资源。
///
/// Code Logic（这个函数做什么）:
///     显式声明 tauri 配置和图标文件为 Cargo build script 依赖，再委托
///     `tauri_build::build()` 生成 Tauri 所需的编译期上下文。
fn main() {
    println!("cargo:rerun-if-changed=tauri.conf.json");
    println!("cargo:rerun-if-changed=icons/32x32.png");
    println!("cargo:rerun-if-changed=icons/128x128.png");
    println!("cargo:rerun-if-changed=icons/128x128@2x.png");
    println!("cargo:rerun-if-changed=icons/icon.icns");
    println!("cargo:rerun-if-changed=icons/icon.ico");
    println!("cargo:rerun-if-changed=icons/tray-icon.png");
    tauri_build::build()
}
