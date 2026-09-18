# Workbench 终端把文件交给 Agent

**Date:** 2026-09-18
**Status:** Approved for implementation（用户：推荐方案；拖入为主，粘贴文件同行，远端落临时目录）

## Goal

桌面工作台终端支持像本机一样把文件交给正在跑的 Agent：从 Finder 拖入（主路径）或 Cmd+V 粘贴非图片文件。本机项目注入原生路径；远端项目先拷到 owning device 临时目录，再按身份表路径语法写入 PTY。

## Decisions

| Topic | Choice |
| --- | --- |
| Primary gesture | Finder 拖到终端 pane |
| Paste files | 与拖入同一通道；图片仍走 paste-image（图优先） |
| Typed `@路径` | 不拦截；表示 Agent 机器上已有路径 |
| Remote landing | `{data_dir}/tmp/agent-files/<session>/<drop-id>/`，不进 git / 不进传输页接收目录 |
| Folder | 保留相对结构；终端注入文件夹根路径 |
| Size | 远端拷贝解码后总量 ≤20 MiB、文件数 ≤256；超限明确失败，无断点续传 |
| Local drag | 不拷贝，注入原路径 |
| Local paste blob | 无原生路径时写入本机临时目录再注入 |
| Mobile | 本轮不做 |

## Architecture

控制端只把路径或粘贴字节交给 sidecar。本机会话：路径直接注入；blob 先落临时目录。远端会话：sidecar 读盘/收 blob，POST `/api/workbench/sessions/attach-files`（capability `workbench.terminal-attach-file.v1`）到 owning device；owner 落临时目录后按 `headless_image_paste` 语法 `write_input`。禁止走 32 KiB 终端输入 WebSocket。缺 capability 为 unsupported。

拖入用 Tauri `onDragDropEvent` 的原生路径，并按 drop 坐标命中终端面板。粘贴拦截非图片 `File`；有 `file://` URI 则当路径。

## Error handling

离线 / 超限 / 路径不存在 / 对端缺能力：写入现有终端错误区。进行中忽略重复拖入。不跟随符号链接。

## Testing

Rust：相对路径清洗、目录收集、跳过 symlink、超限、落盘、注入语法。前端：命中测试、图优先、非图片粘贴、uri-list。Control 路径：`sessions.attachFiles` 走 `workbench/data`。
