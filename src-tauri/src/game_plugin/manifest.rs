//! game.json 清单。
//!
//! Business Logic（为什么需要这个模块）:
//!     每个游戏文件夹用一份小清单声明 id、入口和完成奖励，宿主不猜目录结构。
//!
//! Code Logic（这个模块做什么）:
//!     反序列化 camelCase；entry 默认 index.html；reward 默认 0。

use serde::Deserialize;

fn default_entry() -> String {
    "index.html".into()
}

/// 插件清单字段。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GameManifest {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_entry")]
    pub entry: String,
    #[serde(default)]
    pub reward_minutes: i64,
}
