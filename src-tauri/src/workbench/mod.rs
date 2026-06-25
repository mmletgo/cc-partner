//! workbench — 项目工作台领域模块
//!
//! Business Logic（为什么需要这个模块）:
//!     工作台把“项目文件夹 + 多个普通终端 + 文件树”聚合为一个运行态工作空间。
//!     该模块只承载本机 MVP 能力；局域网远端项目和信任机制后续单独扩展。
//!
//! Code Logic（这个模块做什么）:
//!     导出本机项目、文件系统、PTY sessions 与 DTO 模块。

pub mod dependencies;
pub mod file_content;
pub mod fs;
pub mod git;
pub mod models;
pub mod projects;
pub mod sessions;
