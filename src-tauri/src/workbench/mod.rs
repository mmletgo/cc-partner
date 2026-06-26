//! workbench — 项目工作台领域模块
//!
//! Business Logic（为什么需要这个模块）:
//!     工作台把“项目文件夹 + 多个普通终端 + 文件树”聚合为一个运行态工作空间。
//!     远端项目能力通过 remote_* 模块逐步扩展，与本机工作台能力共享领域边界。
//!
//! Code Logic（这个模块做什么）:
//!     导出本机项目、文件系统、PTY sessions、远端 ID 映射与 DTO 模块。

pub mod dependencies;
pub mod file_content;
pub mod file_preview;
pub mod fs;
pub mod git;
pub mod html_assets;
pub mod models;
pub mod projects;
pub mod remote_ids;
pub mod sessions;
pub mod sqlite_preview;
