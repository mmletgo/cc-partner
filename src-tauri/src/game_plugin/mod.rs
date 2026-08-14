//! 游戏插件：扫描本地目录、校验清单、沙箱读资源。
//!
//! Business Logic（为什么需要这个模块）:
//!     用户把 vibe coding 的游戏丢进 plugin 目录后，大厅要列出并安全打开，
//!     不能执行 npm 或让游戏逃出自己的文件夹。
//!
//! Code Logic（这个模块做什么）:
//!     导出扫描、路径解析与自定义协议；命令层只读配置再调用。

mod manifest;
pub mod protocol;
mod scan;

pub use scan::{list_or_create, resolve_game_asset, scan_game_plugins, GamePluginSummary};
