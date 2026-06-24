# 工作台页面 — 设计文档

- 日期：2026-06-24
- 状态：高保真原型已确认，待转入实现计划
- 原型：`docs/design-demos/workbench-prototype.html`
- 截图：`docs/design-demos/workbench-prototype.png`

## 1. 背景与目标

cc-partner 当前已经具备局域网设备发现、SSH 目标管理、Claude Code 资产管理、Claude 历史采集、Prompt 管理和 Tauri IPC 能力，但还缺少一个围绕“项目文件夹”的运行态工作空间。

工作台页面的目标是让用户在 cc-partner 内指定一个项目文件夹，并在该项目上下文中管理多个 Claude Code 终端。项目文件夹可以来自本机，也可以来自局域网内运行 cc-partner 的设备。第一期重点解决“项目选择 + 多 Claude 终端 + 当前会话状态 + 可交互项目文件夹”这条主流程；文件预览留到下一期在同一右侧项目文件夹区域扩展。

## 2. 用户价值

1. 用户可以把 Claude Code 的多个终端会话收束在一个项目工作台里，不必在系统终端、Finder 和 cc-partner 页面之间来回切换。
2. 用户可以在本机和局域网设备之间用同一种交互打开项目，不需要先记住对端 IP 或手动拼 SSH 命令。
3. 用户可以边看终端边操作项目文件树，为下一期图片、代码、Markdown 预览打基础。
4. 用户可以明确看到当前终端会话属于哪个设备、哪个项目路径、运行多久、是否断线或退出。

## 3. 范围

### 3.1 包含

- 新增一级页面「工作台」。
- 支持添加/选择项目文件夹：
  - 本机项目文件夹。
  - 局域网设备上的远端项目文件夹。
- 支持在当前项目中打开多个 Claude Code 终端会话。
- 支持会话 tab 与终端 pane 分屏：
  - 单窗。
  - 双列。
  - 四宫格。
- 右侧项目面板只显示两类信息：
  - 当前会话状态。
  - 可交互项目文件夹。
- 项目文件夹第一期支持：
  - 文件树展示。
  - 刷新。
  - 新建文件。
  - 新建文件夹。
  - 重命名。
  - 删除。
  - 复制相对路径。
  - 选中文件/文件夹后展示基础元信息。
- 远端项目通过对端 cc-partner 执行终端和文件系统操作，本机前端不直接 SSH 到远端。

### 3.2 不包含

- 第一期不做文件内容预览。
- 第一期不做图片预览、代码高亮、Markdown 渲染。
- 第一期不做 Git diff 面板。
- 第一期不做 Claude CLI 配置面板。
- 第一期不在右侧显示 AGENTS.md / CLAUDE.md 状态。
- 第一期不显示事件日志。
- 第一期不做会话持久回放和完整日志归档。
- 第一期不做普通 shell 终端入口，除非实现时确认成本很低；默认只开放 Claude Code 终端。

## 4. 信息架构

页面采用三栏结构：

```text
工作台
├─ 左侧项目栏
│  ├─ 最近项目
│  ├─ 本机项目
│  ├─ 局域网设备项目
│  └─ 添加项目
│
├─ 中央终端区
│  ├─ 顶部项目上下文
│  ├─ 会话 tabs
│  ├─ 终端分屏 panes
│  └─ 布局切换：单窗 / 双列 / 四宫格
│
└─ 右侧项目面板
   ├─ 当前会话状态
   └─ 项目文件夹
```

三栏职责必须保持清晰：

- 左侧只负责项目切换，不展示文件树。
- 中央只负责终端工作区，不塞文件预览。
- 右侧只负责当前会话状态与项目文件夹，不显示配置、日志或说明性内容。

## 5. 交互设计

### 5.1 添加项目

用户点击左侧「添加项目」后，进入添加项目流程：

1. 选择项目来源：
   - 本机。
   - 局域网设备。
2. 本机项目使用系统目录选择器选择文件夹。
3. 局域网项目选择在线设备后，第一期可以先支持手动输入路径；如果实现成本可控，再提供远端目录浏览器。
4. 添加成功后进入该项目工作台，并将项目加入最近项目列表。

项目记录至少包含：

- `id`
- `name`
- `deviceId`
- `deviceName`
- `kind: local | remote`
- `path`
- `lastOpenedAt`

### 5.2 切换项目

用户在左侧项目栏点击项目后：

- 顶部项目上下文更新为当前设备与路径。
- 中央终端区展示该项目关联的会话列表。
- 右侧项目文件夹刷新为该项目根目录。
- 如果项目设备离线，中央终端区进入不可启动状态，右侧文件树显示离线提示。

### 5.3 新建 Claude 终端

用户点击「新建 Claude 终端」后：

1. 后端在当前项目路径下启动交互式 PTY。
2. 默认命令为用户设置中的 Claude CLI 路径，命令形态为交互式 `claude`。
3. 新会话自动成为当前焦点，出现在 session tabs 中。
4. 右侧当前会话状态同步更新。

启动失败时，当前 pane 显示失败原因，并允许用户关闭该会话。失败信息不进入右侧检查器，以免右侧从“状态 + 文件夹”膨胀成日志面板。

### 5.4 会话切换与分屏

会话 tab 用于切换焦点会话，终端 pane 用于并排工作。

布局规则：

- 单窗：只展示焦点会话。
- 双列：展示焦点会话 + 最近使用的另一个会话。
- 四宫格：最多展示 4 个会话。

右侧“当前会话状态”始终跟随焦点会话，而不是跟随鼠标悬停的 pane。

### 5.5 当前会话状态

右侧当前会话状态只展示：

- 会话名。
- 所属设备。
- 项目名。
- 项目路径。
- 命令。
- 状态：启动中 / 运行中 / 已退出 / 断线。
- 运行时长。
- 操作：
  - 重命名。
  - 重启。
  - 停止。
  - 关闭。

状态展示必须紧凑，避免占据右侧项目文件夹的垂直空间。

### 5.6 项目文件夹

右侧项目文件夹第一期是可交互文件树，不是静态目录说明。

交互：

- 点击文件或文件夹：选中该节点，并在底部显示基础元信息。
- 文件夹可展开/收起。
- 刷新：重新读取当前目录。
- 新建文件/文件夹：在当前选中目录下创建。
- 重命名：对当前选中节点执行。
- 删除：对当前选中节点执行，需确认。
- 复制相对路径：复制相对项目根目录的路径。

选中节点基础元信息：

- 名称。
- 类型。
- 相对路径。
- 大小。
- 修改时间。

下一期文件预览接入同一区域底部，不新增独立右侧 tab：

```text
项目文件夹
├─ 文件树
└─ 预览区
   ├─ 图片预览
   ├─ 代码高亮
   └─ Markdown 渲染
```

## 6. 视觉设计

高保真原型已确认，后续实现应复用现有 cc-partner 设计系统：

- 背景使用 `--bg`。
- 面板使用 `--surface` / `--surface-warm`。
- 强调色使用 `--accent` / `--accent-soft`。
- 卡片圆角不超过现有 `--radius-md` / `--radius-lg` 约定。
- 页面不做营销式 hero，不做大面积装饰图，不使用渐变球或装饰性插画。
- 终端 pane 可以使用深色终端区域，但整体页面不能变成一套新的深色主题。
- 信息密度应接近 Devices / ClaudeCodeAssets 页面，而不是落地页。

原型中的可取元素：

- 左侧项目卡片显示设备、路径和运行中会话数量。
- 中央顶部显示当前项目与路径。
- 中央使用终端 tab + 分屏 pane。
- 右侧项目面板顶部只做一句短说明。
- 文件树底部预留下一期预览入口。

## 7. 后端能力拆分

不要复用 `claude_cli.rs` 实现交互式工作台。`claude_cli.rs` 面向一次性 pure/headless 结构化调用，而工作台需要长期运行的 PTY 会话。

建议新增两个后端能力域。

### 7.1 `workbench_session`

职责：

- 创建 Claude Code 终端会话。
- 管理 PTY 生命周期。
- 写入用户输入。
- 处理 resize。
- 关闭会话。
- 推送输出事件。
- 返回会话状态。

建议 Tauri 命令：

- `list_workbench_sessions(projectId?)`
- `create_workbench_session(projectId, kind)`
- `write_workbench_session_input(sessionId, data)`
- `resize_workbench_session(sessionId, cols, rows)`
- `close_workbench_session(sessionId)`
- `rename_workbench_session(sessionId, name)`

建议事件：

- `workbench:terminal-output`
- `workbench:terminal-status`
- `workbench:terminal-exit`

输出事件需要携带：

- `sessionId`
- `chunk`
- `seq`
- `ts`

`chunk` 建议使用 base64 或明确的 UTF-8 文本策略，避免 ANSI/control bytes 在 JSON 边界被破坏。

### 7.2 `workbench_fs`

职责：

- 列目录。
- 返回文件元信息。
- 新建文件/文件夹。
- 重命名。
- 删除。
- 复制/返回相对路径。
- 下一期读取预览内容。

建议 Tauri 命令：

- `list_workbench_dir(projectId, path)`
- `create_workbench_file(projectId, parentPath, name)`
- `create_workbench_dir(projectId, parentPath, name)`
- `rename_workbench_path(projectId, path, newName)`
- `delete_workbench_path(projectId, path)`
- `get_workbench_path_info(projectId, path)`

下一期新增：

- `read_workbench_preview(projectId, path)`

预览返回类型预留：

- `image`
- `code`
- `markdown`
- `unsupported`
- `tooLarge`

## 8. 远端项目设计

本机项目：

- 本机 Tauri 命令直接访问本机文件系统。
- 本机 PTY 在指定 cwd 中启动 `claude`。

局域网远端项目：

- 本机前端仍调用本机 Tauri 命令。
- 本机后端发现项目属于远端设备后，通过 P2P HTTP 转发到对端 cc-partner。
- 对端 cc-partner 在对端机器上执行文件系统操作和 PTY 会话。
- 输出再由对端流式返回本机，本机通过 Tauri event 转发给 React。

远端 P2P 端点建议：

- `GET /api/workbench/fs/list`
- `POST /api/workbench/fs/create-file`
- `POST /api/workbench/fs/create-dir`
- `POST /api/workbench/fs/rename`
- `POST /api/workbench/fs/delete`
- `POST /api/workbench/sessions`
- `POST /api/workbench/sessions/{id}/input`
- `POST /api/workbench/sessions/{id}/resize`
- `DELETE /api/workbench/sessions/{id}`
- `GET /api/workbench/sessions/{id}/stream`

远端能力第一期可以分阶段落地：

1. 本机项目完整可用。
2. 远端项目路径手输 + 文件树可读。
3. 远端 Claude 终端可运行。
4. 远端文件增删改可用。

## 9. 安全边界

工作台远端终端等价于允许局域网设备在本机或对端执行命令，安全等级高于文件同步和 Prompt 同步。远端能力必须有信任机制。

最低要求：

- 首次远端启动终端时，对端弹窗确认。
- 远端可拒绝某台设备的请求。
- 信任关系落库，至少包含 `trustedDeviceId` 与随机 token。
- 仅受信设备可创建远端终端会话。
- 默认不允许任意局域网设备直接执行命令。
- 远端可以配置允许访问的项目根目录。

第一期如果只实现本机终端，可以先不做完整信任系统；一旦实现远端终端，信任系统必须同时进入范围。

## 10. 前端实现建议

新增页面：

- `web/src/pages/Workbench/Workbench.tsx`
- `web/src/pages/Workbench/Workbench.module.css`
- `web/src/pages/Workbench/index.ts`

新增 API：

- `web/src/api/workbench.ts`

新增业务组件可以按职责拆分：

- `components/domain/WorkbenchProjectList`
- `components/domain/WorkbenchTerminalTabs`
- `components/domain/WorkbenchTerminalPane`
- `components/domain/WorkbenchSessionStatus`
- `components/domain/WorkbenchFileTree`

如果组件只在 Workbench 页面使用，也可以先放在 `pages/Workbench/` 内部，待复用需求明确后再提到 `components/domain`。不要为了抽象而提前制造跨层依赖。

终端渲染建议：

- 前端可以引入成熟 terminal renderer，例如 xterm.js。
- 如果引入 xterm.js，需要确认 Vite/Tauri 下构建体积与样式加载方式。
- 不建议手写 ANSI parser。

React 规则：

- 所有 hooks 必须在 early return 之前。
- 页面文案走 i18n，不在组件硬编码中英文用户可见文本。
- 样式颜色、字体、间距、圆角、阴影必须走 token。

## 11. 数据模型建议

### 11.1 WorkbenchProject

```ts
interface WorkbenchProject {
  id: string;
  name: string;
  kind: 'local' | 'remote';
  deviceId: string;
  deviceName: string;
  path: string;
  lastOpenedAt: string;
  createdAt: string;
  updatedAt: string;
}
```

### 11.2 WorkbenchSession

```ts
interface WorkbenchSession {
  id: string;
  projectId: string;
  name: string;
  command: string;
  status: 'starting' | 'running' | 'exited' | 'disconnected';
  cols: number;
  rows: number;
  startedAt: string;
  exitedAt?: string;
  exitCode?: number;
}
```

### 11.3 WorkbenchFileNode

```ts
interface WorkbenchFileNode {
  name: string;
  path: string;
  kind: 'file' | 'dir';
  size?: number;
  modifiedAt?: string;
  children?: WorkbenchFileNode[];
}
```

## 12. 分期计划

### 第一期

- 新增工作台页面。
- 本机项目选择与最近项目。
- 本机 Claude Code 多终端。
- 会话 tabs。
- 单窗 / 双列 / 四宫格布局。
- 当前会话状态。
- 右侧文件树与基础文件操作。
- 项目文件元信息。

### 第二期

- 局域网远端项目浏览。
- 远端 Claude Code 终端。
- 远端信任确认与 token。
- 远端断线状态与重连。

### 第三期

- 文件预览：
  - 图片。
  - 代码。
  - Markdown。
- 从 Prompt 库发送内容到当前终端。
- 会话日志保存。
- 与 Claude 历史页按项目联动。

## 13. 测试与验收

### 13.1 前端

- `cd web && npm run build`
- 工作台页面 Playwright 验证：
  - 选择项目。
  - 新建会话。
  - 切换会话 tab。
  - 切换布局。
  - 点击文件树节点。
  - 文件操作按钮可触发对应 API。

### 13.2 后端

- `cd src-tauri && cargo test`
- `cd src-tauri && cargo clippy -- -D warnings`
- PTY 会话单测/集成测试：
  - 创建会话。
  - 写入输入。
  - resize。
  - stop。
  - exit 状态。

### 13.3 手测

- 本机项目中启动多个 Claude Code 终端。
- 关闭单个会话不影响其他会话。
- 切换项目后，右侧文件树和会话状态同步更新。
- 设备离线时远端项目显示断线状态，不丢失页面。
- 删除文件/文件夹前有确认，不误删。

## 14. 开放问题

1. 第一期是否允许普通 shell 终端，还是严格只允许 Claude Code 终端。
2. 远端项目第一版是否需要图形化目录浏览，还是先支持手动输入路径。
3. 项目列表是否需要跨设备同步，还是只保存在本机。
4. 会话退出后是否保留最后输出，保留多久。

这些问题不阻塞设计确认，可以在实现计划阶段逐项定边界。
