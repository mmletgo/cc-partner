# Workbench Remote Projects Design

- 日期：2026-06-26
- 状态：方案已确认，待转入实现计划

## 1. 背景

Workbench 当前已经支持本机项目、Git worktree、tmux-backed terminal window/pane、文件树、文件内容浏览/编辑、Git 提交树和 Prompt 优化浮层。现有 PRD 与代码仍把 Workbench 定位为本机目录或已挂载局域网目录，远端 cc-partner 原生项目、远端 PTY 和远端文件预览尚未实现。

用户确认的新目标是：在可信局域网内，直接从本机 Workbench 浏览远端设备的文件系统，选择远端设备上的项目文件夹，并让后续文件、Git、worktree、终端和 Prompt 优化操作都在远端设备上执行。本机只负责展示 UI 和转发控制流，体感应尽量与本地项目一致。

## 2. 设计原则

1. 可信局域网直连：同一局域网内发现的 cc-partner 设备默认可信，不做配对、token、授权弹窗或允许范围配置。
2. 远端实时代理优先：第一版不做批量同步副本，不把远端项目复制到本机后开发。
3. UI 复用：前端继续使用同一套 Workbench 页面、文件工作区、终端和 Git 历史组件，不复制一套远端 UI。
4. 执行位置明确：remote 项目的文件 IO、Git、worktree、tmux/PTY、Claude CLI Prompt 优化都在远端设备执行。
5. 技术边界保留：虽不做安全授权，仍要做路径规范化、坏路径拒绝、目录存在校验、危险操作确认、dirty tab 防丢失和请求 stale guard。

## 3. 用户体验

### 3.1 添加远端项目

用户在侧栏项目文件夹区点击 `+` 后，不再只能打开本机系统目录选择器，而是进入来源选择：

1. 本机项目。
2. 局域网设备项目。

选择本机项目时维持现有系统目录选择器。

选择局域网设备项目时：

1. 展示当前 mDNS 已发现的在线设备。
2. 用户选择设备。
3. 打开远端目录选择器。
4. 用户浏览远端设备目录。
5. 用户选中一个项目文件夹。
6. 本机保存 remote project shortcut。
7. Workbench 自动进入该远端项目。

远端设备不需要预先在自己的 Workbench 中添加或授权该项目。本机选择路径后，会通过远端 HTTP 能力让远端设备自动创建或复用对应本机项目记录。

### 3.2 远端目录选择器

远端目录选择器是一个应用内目录浏览器，用来替代系统原生目录选择器在远端设备上的能力。

目录选择器需要支持：

- 设备切换。
- 远端 roots 展示。
- 进入目录。
- 返回上级目录。
- 刷新当前目录。
- 显示目录名、路径、修改时间和基础可读状态。
- 标识目录是否 Git 仓库。
- 选中目录后作为 Workbench 项目打开。

远端 roots 建议：

- macOS/Linux：`~`、`~/Desktop`、`~/Documents`、`~/Downloads`、`~/web_project`、`~/projects`、`~/workspace`、`/`。
- Windows：用户目录、桌面、文档、下载、可用盘符列表。

roots 可以返回不存在项的过滤结果，避免 UI 展示无效入口。

### 3.3 远端项目进入后的体感

远端项目进入后，用户仍看到同一套 Workbench：

- 左侧项目卡片显示远端设备名和远端路径。
- 顶部 worktree 管理层列出远端项目的主 worktree 和已有 worktree。
- 中央终端区创建的是远端设备上的 terminal window/pane。
- 右侧文件树读取的是远端 active worktree。
- 文件 tab 打开的内容来自远端。
- 保存、新建、重命名、删除写入远端文件系统。
- Git commit/push/merge 在远端 worktree 执行。
- Prompt 优化在远端 active worktree cwd 下调用远端 Claude CLI，并把结果流式写入远端 active session。

远端设备离线时，项目 shortcut 可以保留，但进入项目后应显示设备离线状态，禁止新建终端、文件写入、Git 操作和 Prompt 优化。已打开文件 tab 标记为不可刷新，避免误以为内容仍是实时状态。

## 4. 架构

### 4.1 项目模型

复用现有 `workbench_projects` 表字段：

- `kind = "local"`：本机项目，保持当前行为。
- `kind = "remote"`：远端项目 shortcut。
- `device_id/device_name`：远端设备身份。
- `path`：远端设备上的绝对项目路径。

需要新增远端映射字段时，优先新增轻量字段或在派生 id 中表达，不要把远端项目复制成本机路径。

建议 ID 命名空间：

- 本机项目保持现有 UUID。
- 远端项目 shortcut 可使用 `remote:{deviceId}:{hash(path)}`。
- 远端 worktree/session 返回本机前先映射为 `remote:{deviceId}:{remoteId}`。

这样前端仍传 `projectId/worktreeId/sessionId`，后端 gateway 可从 ID 判断本地执行还是远端代理。

### 4.2 后端分层

新增三层：

1. `workbench local service`
   - 从现有 `commands/workbench.rs` 抽出本机项目、worktree、session、files、git 的业务实现。
   - Tauri command 和远端 HTTP route 都调用它。

2. `net/routes/workbench.rs`
   - 对外暴露远端目录选择和远端 Workbench 操作。
   - 这些 HTTP handler 在远端设备上执行本机 service。

3. `RemoteWorkbenchClient`
   - 本机 gateway 使用它请求对端 `/api/workbench/...`。
   - 复用现有 `PeerClient` 的 reqwest、base URL 和错误映射风格。

Tauri command 层变为 gateway：

- local 项目：调用 local service。
- remote 项目：解析 `deviceId`，找到 mDNS 设备 base URL，调用 `RemoteWorkbenchClient`。

### 4.3 远端 HTTP 能力

目录选择阶段：

- `GET /api/workbench/fs/roots`
  - 返回远端 roots。

- `POST /api/workbench/fs/list`
  - 入参：`{ path }`
  - 返回：目录一级子项，优先目录，附带 `isGitRepo` 等轻量标识。

- `POST /api/workbench/fs/info`
  - 入参：`{ path }`
  - 返回：路径是否存在、是否目录、是否可读、是否 Git 仓库、建议项目名。

- `POST /api/workbench/projects/open`
  - 入参：`{ path }`
  - 远端自动创建或复用本机项目记录。
  - 返回远端 `WorkbenchProjectDto`。

项目工作阶段：

- projects：touch/remove 可按 remote shortcut 处理，远端项目记录删除与本机 shortcut 删除要分清。
- worktrees：list/create/commit/push/merge/remove。
- git：list commits。
- files：list dir/info/open/save/format/sqlite/html asset/create/rename/delete。
- sessions：list/create/write/resize/focus/focused/split/close pane/close session/rename。

可以先实现远端目录选择与只读文件树，再逐步补齐全部 Workbench API。

### 4.4 远端终端事件桥接

远端终端不能只靠 request/response。需要一个从远端到本机的事件流：

1. 本机为每个远端设备建立 Workbench event stream。
2. 远端把 `workbench:terminal-output`、`workbench:terminal-status`、`workbench:merge-progress` 写入事件流。
3. 本机收到后映射 `sessionId/projectId/worktreeId`。
4. 本机重新 emit 当前前端已消费的 Tauri 事件名。

第一版可选方案：

- SSE / NDJSON over HTTP：实现简单，适合单向输出流。
- WebSocket：后续双向控制也可复用，但引入复杂度更高。

推荐第一版用 SSE 或 NDJSON。输入、resize、focus 等控制仍走普通 HTTP request。

### 4.5 Prompt 优化

远端项目时，Workbench Prompt 优化不能在本机调用 Claude CLI。正确流程：

1. 前端仍调用现有 Prompt 优化 API。
2. 本机 gateway 判断 active project 为 remote。
3. 本机把 prompt、targetLanguage、remote workingDirectory、remote active sessionId 转发到远端。
4. 远端在 remote worktree cwd 下调用 Claude CLI。
5. 远端把生成文本流式写入 remote session。
6. 远端 session 输出再通过事件流桥接回本机 xterm。

## 5. 技术边界

虽然不做局域网安全授权，仍必须保留这些技术边界：

- 远端目录浏览拒绝空路径、非法路径、不可读路径。
- 文件操作继续限制在项目 active worktree 根内。
- 删除文件、删除目录、删除 worktree、关闭 dirty tab 继续要求确认。
- 保存文本继续使用 baseHash 乐观锁。
- HTML/Markdown 资源预览继续拒绝外链、绝对路径、根外路径和跨根 symlink。
- 请求返回必须比对当前 `projectId/worktreeId/path`，旧响应不得污染新项目 UI。
- 远端设备离线或 HTTP 失败时，错误要映射为可操作提示，而不是把 reqwest 底层错误直接抛给用户。

## 6. 第一版范围

第一版包含：

- 远端设备选择。
- 远端目录选择器。
- 直接选择远端设备任意项目文件夹。
- 本机保存 remote project shortcut。
- 远端 worktree 列表与 Git 状态。
- 远端文件树与文件内容浏览/编辑。
- 远端 Git 历史。
- 远端 terminal window/pane。
- 远端 Prompt 优化流式写入 active session。

第一版不包含：

- 批量同步副本。
- 离线编辑远端项目。
- 远端全文搜索。
- 远端 Git diff 面板。
- 远端 PR 创建。
- 设备配对、token、授权弹窗和允许范围配置。

## 7. 实施顺序

1. 抽出本机 Workbench service，确保 Tauri command 仍通过现有测试。
2. 新增远端目录选择 HTTP routes 和 client。
3. 改造前端添加项目流程，接入远端设备与目录选择器。
4. 引入 remote project shortcut 与 ID 映射。
5. 让 worktree、files、git 的只读能力走远端代理。
6. 补齐文件写入和路径 CRUD。
7. 接入远端 terminal session API。
8. 接入远端终端事件流桥接。
9. 接入远端 commit/push/merge 与 merge-progress 事件。
10. 接入远端 Prompt 优化。

## 8. 风险

- 终端事件流和 session ID 映射是最大复杂点，需要先做小范围端到端验证。
- Workbench 页面已有较多状态，前端改造应尽量收敛在项目添加流程和 API gateway，不复制页面逻辑。
- 远端设备离线、重启、IP 变化时，需要用 deviceId 重新解析当前 mDNS 设备，不能持久化旧 IP。
- 远端路径跨平台展示要按远端平台语义处理，不能在本机尝试 canonicalize 远端路径。
- 大文件预览和保存限制应沿用现有 Workbench 文件能力，避免远端 HTTP body 超限。
