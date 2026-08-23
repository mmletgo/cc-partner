# 浏览中新建文件夹并打开为项目

- 日期：2026-08-24
- 状态：已确认（用户决策，待实现）
- 上位文档：
  - [`2026-06-24-workbench-design.md`](./2026-06-24-workbench-design.md)
  - [`2026-06-26-workbench-remote-projects-design.md`](./2026-06-26-workbench-remote-projects-design.md)
  - [`2026-08-24-mobile-project-management-design.md`](./2026-08-24-mobile-project-management-design.md)
- PRD：[`docs/prd.md`](../../prd.md)「添加项目文件夹」与移动端 Workbench 添加目录条款

## 1. 文档地位

本 Spec 覆盖 Workbench **添加项目**路径上的两件事：

1. 在本机或局域网设备的**指定父目录**里新建一层文件夹。
2. 将目录打开为项目；若打开时该目录按 §5 判定为空，则在**目录所在设备**上 `git init`（不提交、不写 README）。

它覆盖（不再有效）下列旧条款：

- 2026-06-26：桌面「本机项目继续使用系统目录选择器」。
- PRD 同句：本机项目使用系统目录选择器。

不推翻：固定 LAN 无身份鉴权、remote shortcut 模型、项目内 `files/create-dir`（已打开项目的文件树）、Settings 等其它 `chooseDir` 用途。

冲突优先级：本补丁中的用户确认决策 → 本补丁 → 更早 Workbench / 移动端项目管理 Spec / PRD。

## 2. 已确认决策

| # | 决策 | 说明 |
|---|------|------|
| D1 | 四条入口同一套浏览 | PC 本机、PC 局域网、手机看主机目录、手机看局域网设备，都用应用内目录浏览。 |
| D2 | PC 本机不用系统框 | 添加本机项目改为应用内选择器。系统「选择文件夹」仍可用于 Settings 等非 Workbench 添加项目路径。 |
| D3 | 建完只选中 | 新建成功后刷新当前列表并自动选中新文件夹，**不**自动进入工作台。用户再点「打开项目」。 |
| D4 | 新建不 init | `mkdir` 只创建空目录。 |
| D5 | 打开时空目录才 init | 任何「打开为项目」路径（含早就存在的空目录，不限刚新建）在登记项目前按 §5 判断；空则 `git init`。 |
| D6 | 空 = 可忽略系统垃圾 | 子项只属于 `{.DS_Store, Thumbs.db, desktop.ini, .localized}` 时仍算空。已有 `.git`（文件或目录）或任何其它文件/子目录则不 init。 |
| D7 | 浏览层 mkdir，打开时 init | 不把「没有就创建」绑在 open 上。现有项目内 `files/create-dir` 不承担项目外建目录。 |
| D8 | 不合并桌面/手机选择器组件 | 桌面 Dialog 与手机 Drawer 各自加「新建文件夹」。可抽纯 helper，不抽成单一 UI 组件。 |

## 3. 用户结果

完成后，用户能够：

- 在 PC 点「添加本机项目」，浏览本机目录，在当前目录新建文件夹，选中后点「打开项目」进入工作台。
- 在 PC 点「局域网设备」，在对端当前目录同样新建，再确认打开（本机写 remote shortcut，对端创建/复用 local 项目记录）。
- 在 `/mobile` 对主机目录或局域网对端做同一套：新建 → 自动选中 → 打开。
- 打开看起来为空的目录时，该设备上出现无提交的 Git 仓库；已有真实内容或已是仓库的目录行为与现在一致。
- 名称非法或已存在时留在新建对话框，不覆盖、不进入工作台。

## 4. 范围

### 4.1 包含

- 浏览层单层 `mkdir`（父目录必须已存在且为目录）。
- 桌面本机应用内选择器（替换 Workbench 添加本机项目的 `chooseDir`）。
- 桌面远端选择器与手机本机/局域网选择器的「新建文件夹」+ 名称 Dialog。
- 打开为项目时的空目录 `git init`（本机 `add_workbench_project` / P2P `projects/open` / 手机打开主机目录 / 对端经 `open_project` 登记，全部走同一 owner 函数）。
- 新 P2P/Mobile 写路由、能力 token、前端 API、i18n、PRD 同步。

### 4.2 不包含

- 一次创建多层路径（`create_dir_all` / `a/b/c`）。
- 自动 `git add` / commit / README / `.gitignore` 脚手架。
- 把桌面 `WorkbenchRemoteProjectPicker` 与手机 `MobileProjectPicker` 收成一个组件。
- 保留 Workbench 添加本机项目的系统目录框双入口。
- 新的 LAN 鉴权、权限矩阵、capability token 当授权。
- Settings 或其它非添加项目场景的 `chooseDir`。
- 项目文件树里已有的新建文件夹（`files/create-dir`）行为变更。

## 5. 空目录与 git init

判定发生在 **owner 设备**、路径已 `canonicalize` 且确认为目录之后、写 `workbench_projects` 之前。

```
若 path/.git 存在（文件或目录，不要求合法仓库）→ 不 init
否则列出 path 的一级子项：
  若存在名称不在垃圾集合中的子项 → 不 init
  否则 git init（cwd = canonical path，不指定 --initial-branch，尊重该机 git 配置）
```

垃圾集合（大小写敏感，与 `DirEntry` 的 `file_name()` 精确相等）：`.DS_Store`、`Thumbs.db`、`desktop.ini`、`.localized`。只比名称，不论该项是文件还是目录。

规则：

- 不提交、不写 README、不改已有文件。
- `git` 不在 PATH 或 `git init` 非 0 退出：这次打开失败，**不** upsert 项目记录。目录（含刚 mkdir 的空文件夹）留在磁盘。错误文案说明无法初始化 Git 仓库。
- 非空或不 init 的失败不得误创建 `.git`。
- 已是仓库：不 init，继续打开（与现在一致）。

## 6. 架构

### 6.1 两段能力

```
选择器                  owner 磁盘                 项目表
浏览 roots/list/info  →  （只读）
新建文件夹            →  mkdir 一层                 不写项目
打开项目              →  空则 git init  →  upsert local 项目
远端打开              →  对端执行上两步，本机只写 remote shortcut
```

`mkdir` 与 `open` 失败彼此独立：建成功但未打开 = 磁盘上多个空文件夹，不是项目。打开失败不删刚建的目录。

### 6.2 共享 helper（Rust）

放在 `workbench/remote_directory.rs`（或与之同层的 browse helper，避免把项目外路径写进 `workbench/fs.rs` 的项目根约束里）：

1. `create_browse_dir(parent: &Path, name: &str) -> WorkbenchRemotePathInfo`（或等价 DTO）
   - 名称校验与项目内 `validate_child_name` **同一规则**：非空、无 `/` `\`、不是 `.` / `..`；允许非 ASCII。把该函数抽到双方可引用处，禁止复制一份略不同的规则。
   - 父路径必须存在且为目录。
   - 目标已存在（含文件占名）→ 业务错误「目标路径已存在」。
   - `fs::create_dir`（不是 `create_dir_all`）。
   - 返回新目录的 list/info 同形字段（name/path/kind/modified_at/is_git_repo=false）。

2. `dir_is_empty_for_git_init(path: &Path) -> bool` — §5。

3. `git_init_if_empty(path: &Path) -> Result<(), AppError>` — §5；由 `add_local_workbench_project_from_path` 在 canonicalize 之后调用。

现有 `workbench/fs.rs::create_dir` 仍只服务已打开项目的相对路径，禁止拿它做浏览层 mkdir。

### 6.3 协议与命令

**对端 owner HTTP（与现有 fs 浏览并列，不是 `files/*`）：**

| 方法 | 路径 | 副作用 | retry class |
|------|------|--------|-------------|
| POST | `/api/workbench/fs/create-dir` | 在绝对父路径下建一层目录 | `requires-idempotency-key`（本轮仍无 dedupe key；客户端不得自动重试，与 `files/create-dir` 相同） |

- Body camelCase：`{ parentPath, name }`。
- 与 `POST /api/workbench/files/create-dir`（`projectId` + 项目相对 `parentPath`）**不是同一条路由**。
- 能力 token：`workbench.fs.create-dir.v1`，与路由同发，列入 `server_protocol_info()` 字典序。协议协商，不是鉴权。
- 缺能力：选择器隐藏「新建文件夹」或点按后明示不支持，禁止假装成功。
- 按 `src-tauri/AGENTS.md` 新增路由 7 步清单更新 `docs/p2p-protocol.md` + inventory 脚本。

**手机（主机）：**

| 方法 | 路径 | 含义 |
|------|------|------|
| POST | `/api/mobile/workbench/fs/create-dir` | 在提供 `/mobile` 的主机上 mkdir |
| POST | `/api/mobile/workbench/remote/create-dir` | `{ deviceId, parentPath, name }`，主机经 `RemoteWorkbenchClient` 调对端 `/api/workbench/fs/create-dir` |

**桌面 Tauri / sidecar control（与现有 remote_* 平行）：**

- 本机浏览：`list_workbench_fs_roots` / `list_workbench_fs_dir` / `get_workbench_fs_path_info` / `create_workbench_fs_dir`。实现复用 `remote_directory` helper，走现有 GUI→sidecar 代理，不经 P2P 打本机。
- 远端：`create_workbench_remote_fs_dir(deviceId, parentPath, name)`，client 调对端 `fs/create-dir`。

`RemoteWorkbenchClient` 增加 `create_browse_dir`，与 `roots/list/info` 同一套 timeout/错误映射。

### 6.4 前端

**桌面**

- `WorkbenchRemoteProjectPicker` 增加 `source: 'local' | 'remote'`（或等价 prop）。`local` 跳过设备列表，roots/list/info/mkdir 走本机 fs 命令；打开走 `projects.add`。
- Workbench 添加本机项目（`WorkbenchProjectRail` / Launch / 空态 CTA）打开该选择器的 local 模式，不再调用 `chooseAndAddProject`→`configApi.chooseDir()`。`chooseAndAddProject` 可删除或收成测试替身；不得在添加本机项目 CTA 上保留系统框。
- `remote` 模式在当前目录工具条增加「新建文件夹」。对端缺 `workbench.fs.create-dir.v1` 时不展示该按钮。
- 名称用现有 `Dialog` + `Input`，确认/取消；busy 时禁止关选择器（与打开项目中相同）。
- 成功：重新 `listDir` 当前路径，把 `selectedPath` 设为新目录绝对路径，触发 pathInfo；「打开项目」仍走现有 `canOpen*` 门闩。

**手机**

- `MobileProjectPicker` 的 local 与 lan-browse 同样加「新建文件夹」+ Dialog。lan 走 `remote/create-dir`，本机走 `fs/create-dir`。
- 不把桌面选择器塞进 Drawer。

**共享（纯函数，非组件）**

- 名称校验可与后端规则对齐的前端预检（空/分隔符/`.`/`..`），最终以后端错误为准。
- reducer 增加 createBusy / createError，与 openBusy 互斥：任一段 busy 时禁用另一段。

## 7. 数据流

### 7.1 新建

1. 当前 `currentPath` 非空且非 busy。
2. 用户确认名称。
3. local：owner `create_browse_dir`。remote：对端同一 helper。mobile lan：手机 → 主机 → 对端。
4. 成功返回新路径 → 刷新 entries → `selectedPath = newPath`。
5. 失败：Dialog 内错误，列表不变。

### 7.2 打开

与现在相同，但 owner `add_local_workbench_project_from_path` 增加 §5。远端打开仍是：对端 upsert local 项目（含可能的 init）→ 本机 upsert `kind=remote` shortcut。

## 8. 错误与边界

| 情况 | 行为 |
|------|------|
| 名称为空 / 含分隔符 / `.` / `..` | 拒绝 mkdir，不写盘 |
| 父路径空、不是目录、不可访问 | 拒绝 mkdir |
| 目标已存在 | 拒绝 mkdir，不覆盖、不改成打开 |
| 对端离线 / 传输失败 | 选择器错误；不写 shortcut |
| 对端无 `workbench.fs.create-dir.v1` | 无新建按钮或 unsupported，不回落项目内 `files/create-dir` |
| `git init` 失败 | 打开失败，不登记项目；目录保留 |
| 打开非空或已是 git | 不 init，打开成功（已有行为） |
| 刚 mkdir 未点打开 | 磁盘有空目录，最近项目列表无新项 |
| LAN | 无新鉴权；风险文案沿用现有 LAN 披露 |

mkdir 与项目内 create-dir 一样不可自动重试：超时后用户刷新列表，已存在则不再建。

## 9. 验证

最低限度（实现计划拆任务时不得删掉语义）：

- Rust：`create_browse_dir` 拒绝分隔符、`.`/`..`、父非目录、目标已存在；成功后目录存在且为空。
- Rust：`dir_is_empty_for_git_init` — 真空、仅垃圾文件、含 `.git`、含普通文件、含子目录。
- Rust：`add_local_workbench_project_from_path` — 空目录产生 `.git` 且无 commit；非空不产生 `.git`；已有 `.git` 不二次 init；`git init` 失败不 upsert。
- 前端：local/remote picker 新建成功后 selectedPath 为新目录且不调用 open；名称非法不发请求。
- 手机：local 与 lan 两条 create-dir HTTP 契约（`workbenchHttp` 测试）。
- `node scripts/check-p2p-route-inventory.mjs`；capability 出现在 health 字典序全集。
- 桌面添加本机项目不再触发系统目录框（Rail / Launch CTA 测到打开应用内选择器）。

不在本补丁宣称 L3 真机；质量矩阵若加行保持 `NOT VERIFIED` 直到人工跑过。

## 10. 文档

实现同一改动内更新：

- `docs/prd.md`：本机项目改为应用内浏览；四条入口均可在当前目录新建文件夹后确认打开；打开空目录时 owner `git init`。
- `docs/p2p-protocol.md`：§6.3 三行路由 + 能力说明。
- 根/`web`/`src-tauri` `AGENTS.md` 仅在清单缺入口时补一行（能力 token、组件行为），不把本 Spec 正文搬进指令文件。

## 11. 明确不选的替代

- 打开时「路径不存在就创建」：无法做 D3 确认打开，误路径会直接建目录并打开。
- 先合并桌面/手机选择器再加功能：Dialog/Drawer 壳不同，会把本补丁绑在重构上。
- 创建时 git init：与 D4 冲突；未打开的目录不应变成仓库。
