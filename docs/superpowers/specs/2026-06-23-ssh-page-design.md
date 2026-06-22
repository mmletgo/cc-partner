# SSH 页 设计文档

- 日期：2026-06-23
- 状态：已与用户对齐，待转入实现计划

## 1. 概述与目标

在左侧菜单栏新增「SSH」页，帮助用户：

1. **列出局域网设备清单（IP）**：复用现有 mDNS 设备发现（`list_devices`），额外支持手动填入任意 IP。
2. **为每个连接目标设置用户名/端口**：配置落 SQLite，并基于向量时钟**跨设备同步**（复用项目既有同步基础设施，不重造轮子）。
3. **一键复制 SSH 连接命令**：按目标配置拼出 `ssh ...` 命令写入剪贴板。
4. **提供 SSH 配置指南**：
   - 被连接设备（目标机）如何开启 SSH 服务——mac / Ubuntu / Windows 三端并列展示（mDNS 不上报对方 OS，用户自选）。
   - 本机（连接发起端）如何使用——按本机操作系统只展示对应那一端。

## 2. 范围

### 包含

- 后端新增 `ssh_targets` 表 + repo + 向量时钟合并 + P2P 同步端点 + 4 个 Tauri 命令（`list_ssh_targets` / `upsert_ssh_target` / `delete_ssh_target` / `get_os_info`）。
- 前端新增 SSH 页（连接目标区 + 配置指南区）+ api 模块 + 类型 + 终端图标 + 菜单项 + 路由 + i18n（中英）。
- 配置跨设备同步（完整向量时钟）。
- 复制命令功能。

### 不包含（YAGNI，后续按需）

- 不在后端 spawn 打开本机终端并自动执行 ssh（交互形态已定为「指南 + 复制命令」，由用户在终端粘贴执行）。
- 不做密钥/密码管理、不做免密密钥分发自动化（仅在指南文案里提示 `ssh-keygen` 用法）。
- 不把 SSH target 关联到 mDNS `device_id`（第一阶段用 `host(IP)` 关联，见第 3 节权衡）。
- 不支持同一 host 多账号（host 是主键，一 host 一配置）。

## 3. 数据模型（后端新增表）

```sql
CREATE TABLE IF NOT EXISTS ssh_targets (
  host         TEXT    PRIMARY KEY,        -- IP 或 hostname，逻辑主键
  port         INTEGER NOT NULL DEFAULT 22,-- SSH 端口，默认 22
  username     TEXT    NOT NULL,           -- SSH 用户名（空串 = 用本机默认用户名）
  label        TEXT,                       -- 可选备注
  device_id    TEXT    NOT NULL,           -- 最后修改设备（同步用）
  vector_clock TEXT    NOT NULL,           -- JSON {"device_id": counter}，复用现有向量时钟
  updated_at   TEXT    NOT NULL,           -- ISO，并发 LWW 依据
  created_at   TEXT    NOT NULL,           -- ISO
  deleted      INTEGER NOT NULL DEFAULT 0  -- 软删除，参与同步传播
);
```

> **关联键权衡**：mDNS 设备的 IP（DHCP）可能变化，但第一阶段用 `host(IP)` 关联足够简单一致；若设备换 IP，用户需重新填用户名。设计文档明确此限制，不在第一阶段引入 `device_id` 智能匹配。

### 同步模式选择（重要）

SSH target 同步**对齐 `prompts` / `cc_history` 模式**：

- **DB 是唯一数据源，无外部文件**——因此**不需要** `claude_md` 那种 `reconcile_from_file` 文件对账。
- 合并策略与 `sync/merger.rs` 逐字一致：`compare(remote, local)` 为 `After` → remote 胜；`Before`/`Equal` → local 胜；`Concurrent` → LWW（`updated_at` 更晚胜，相等用 `device_id` 字典序 tie-break）。胜出方取内容，向量时钟合并保留因果历史。软删除（`deleted=1`）照常参与同步传播。

## 4. 后端设计

### 4.1 建表（`lib.rs`）

- 新增常量 `SSH_TARGET_SCHEMA`（上述建表 SQL），在 `init_db` 内 `CC_HISTORY_SCHEMA` 之后、`CLAUDE_MD_SCHEMA` 之前执行（顺序无强依赖，紧跟既有建表块即可）。

### 4.2 `storage/ssh_target_repo.rs`

对齐 `prompt_repo.rs` / `claude_md_repo.rs`，运行期 `sqlx::query`（非宏）：

- `list() -> Vec<SshTargetRow>`：`deleted=0` 全量。
- `get_all_for_sync() -> Vec<SshTargetRow>`：含 `deleted=1`（同步需传播删除）。
- `get_by_host(host) -> Option<SshTargetRow>`。
- `bulk_upsert(&[SshTargetRow])`：`INSERT OR REPLACE`，**合并决策在调用前完成**（upsert 前不判合并）。
- `upsert(row)`：单条便捷封装（命令层用）。
- `soft_delete(host, device_id, now, vc)`：置 `deleted=1` + 推进 vector_clock + 写回。

datetime 透传 `String`，兼容有无时区偏移两种格式（项目约定）；`vector_clock` 用 `serde_json` 紧凑 JSON TEXT。

### 4.3 `sync/ssh_target.rs`

- `merge_ssh_target(local, remote) -> SshTargetRow`：照搬 `merger.rs::merge_prompt` 的决策逻辑（严格领先 / 并发 LWW / 字典序 tie-break / 合并双方 vector_clock），直接 `use crate::sync::vector_clock::{compare, merge}`，**不重复实现向量时钟**。
- `should_update_ssh(local, remote) -> bool`：辅助判断。
- 配 6~7 个单测覆盖：严格领先覆盖、严格落后跳过、并发 LWW、并发时间戳相等字典序 tie-break、删除传播、向量时钟合并保留因果。

### 4.4 P2P 端点 `net/routes/ssh_target_sync.rs`

snake_case 互通（对齐 `claude_md_sync.rs`），`http_server.rs` Router 注册：

- `POST /api/ssh-target/sync/pull`，body `{summaries:[{host, vector_clock}]}` → 返回本端「对端没有 / 本端领先 / 并发」的 `{targets:[SshTargetRow]}`。
- `POST /api/ssh-target/sync/push`，body `{targets:[SshTargetRow]}` → 逐条 `merge_ssh_target` 后 `bulk_upsert`（仅变化才写）→ `{accepted:<count>}`。

### 4.5 `net/peer_client.rs` 扩展

- `ssh_target_pull(addr, port, summaries) -> Result<Vec<SshTargetRow>>`
- `ssh_target_push(addr, port, targets) -> Result<usize>`
- 失败返回 `Err`，调用方 `tracing::warn` 视为空/0 继续，兼容旧版本无此路由的对端。

### 4.6 `sync/engine.rs` 挂载

- `trigger_sync` 对端遍历前**无需** reconcile（无文件源）；每个对端 `sync_with_peer`（prompts）后追加 `sync_ssh_target_with_peer(state, device)`，失败 warn 不阻断，**不计入 `synced` 计数**（计数语义保持「prompts 同步成功」）。
- 单对端流程：health → 本端 summaries（`get_all_for_sync`）→ `ssh_target_pull` 拿回对端需给的 → 逐条 `get` + `merge_ssh_target`（仅变化收集）→ `bulk_upsert` → 重读本地算补集 → `ssh_target_push`。

### 4.7 命令层 `commands/ssh_target.rs`（lib.rs invoke_handler 注册）

返回前端的结构体 `#[serde(rename_all = "camelCase")]`。

- `list_ssh_targets(state) -> Vec<SshTargetDto>`：`repo.list()` → DTO。
- `upsert_ssh_target(state, host, username, port?, label?) -> SshTargetDto`：
  - 读旧记录（若存在）取其 vector_clock；新建则初始化 `{device_id:1}`，更新则 `increment` 本设备计数器。
  - `port` 缺省 22；`username` 缺省空串；`label` 可空。
  - 落库 + 返回 DTO。
- `delete_ssh_target(state, host) -> {ok:true}`：软删除 + `increment` vc。
- `get_os_info() -> OsInfo`：`std::env::consts::OS` 归一化：`macos`→`mac`，`windows`→`windows`，其余（含 `linux`）→`ubuntu`；同时返回 `raw` 原始值。无 state。

### 4.8 数据模型（Rust）

- `models/ssh_target.rs`：
  - `SshTargetRow`（snake_case，DB/同步）：`host/port/username/label/device_id/vector_clock/updated_at/created_at/deleted`。
  - `SshTargetDto`（camelCase，前端）+ `to_dto`。
- `AppState` 扩展 `ssh_target_repo: Arc<SshTargetRepo>`。

## 5. 前端设计

### 5.1 类型 `lib/types.ts`

```ts
export interface SshTarget {
  host: string;
  port: number;
  username: string;
  label?: string;
  updatedAt: string;
}
```

### 5.2 api `api/ssh.ts`

```ts
export const sshApi = {
  list: () => invoke<SshTarget[]>('list_ssh_targets'),
  upsert: (host, username, port?, label?) => invoke<SshTarget>('upsert_ssh_target', {...}),
  remove: (host) => invoke<{ ok: boolean }>('delete_ssh_target', { host }),
  getOsInfo: () => invoke<{ platform: 'mac'|'windows'|'ubuntu'; raw: string }>('get_os_info'),
};
```

### 5.3 页面 `pages/Ssh/Ssh.tsx` + `.module.css`

复用 `pages/Devices/Devices.tsx` 的页面骨架（头部 + 卡片 + 加载/空/错误态 + 容器居中限宽）与 `@/components/primitives`。

**数据合并逻辑**：

- `list_devices` → 实时设备 `Device[]`（`address` = IP）。
- `list_ssh_targets` → 持久配置 `SshTarget[]`。
- 前端把两源合并为「目标列表」：
  - 实时设备：每条用 `device.address` 查 `ssh_targets[host]`，预填 username/port/label；编辑后 `upsert`。
  - 手填区：用户输入 host + username + port（默认 22）+ 可选 label，点「添加」`upsert` 后入列表。
  - 列表额外显示「已配置但当前离线/非实时」的目标（`ssh_targets` 有、`list_devices` 无），可编辑/删除。

**布局**：

```
SSH 标题                  本机系统：macOS [pill]
─────────────────────────────────────────────
连接目标
 [设备] MBP-Work   192.168.1.20  用户名[alice] 端口[22] [复制]
 [设备] Ubuntu-Box 192.168.1.30  用户名[bob__] 端口[22] [复制]
 + 手填 IP[__] 用户名[__] 端口[22] 备注[__] [添加]
─────────────────────────────────────────────
配置指南
 ▸ 本机（连接端）用法 — macOS
     终端执行 ssh user@ip；ssh-keygen 生成密钥免密…
 ▸ 被连接设备如何开启 SSH
     [mac] 系统设置→通用→共享→远程登录
     [Ubuntu] sudo apt install openssh-server && sudo systemctl enable --now ssh
     [Windows] 设置→应用→可选功能→OpenSSH 服务器；Start-Service sshd…
```

- 用户名/端口输入框失焦或回车即 `upsert`（自动保存 + 同步）。
- 复制按钮：按第 7 节格式拼命令，调 `navigator.clipboard.writeText(text)` 写入剪贴板（Tauri webview 支持），给「已复制」反馈。

### 5.4 图标 `lib/icons.tsx`

新增 `TerminalIcon`（菜单项 + 复制按钮复用），风格对齐既有 SVG 图标。

### 5.5 菜单与路由

- `AppShell.tsx` `<nav>` 加 `<NavItem to="/ssh" label={t('nav:ssh')} icon={<TerminalIcon />} />`，位置放在 devices 之后、settings 之前（SSH 与设备/网络同属一类，归在一起）。
- `App.tsx` 在 `<Route element={<AppShell />}>` 内加 `<Route path="/ssh" element={<Ssh />} />`。

### 5.6 i18n

- `i18n/locales/{zh,en}/nav.json` 加 `"ssh"` 菜单项。
- 新建 `i18n/locales/{zh,en}/ssh.json` namespace，放页面标题/区标题/输入框 placeholder/复制反馈/指南全文。**禁止在组件硬编码用户可见中英文字面量**。
- 组件 `useTranslation(['ssh','common','devices'])` + `t('ssh:key')`。

## 6. OS 检测

后端 `get_os_info`（见 4.7）为唯一权威来源，前端不自行推断。指南区「本机用法」按返回的 `platform` 只渲染对应一端；「被连端开启 SSH」三端全展示。

## 7. 复制命令格式

按目标的 `username` 与 `port` 拼接：

| username | port | 命令 |
|---|---|---|
| 非空 | 22 | `ssh {username}@{host}` |
| 非空 | 非 22 | `ssh -p {port} {username}@{host}` |
| 空 | 22 | `ssh {host}` |
| 空 | 非 22 | `ssh -p {port} {host}` |

## 8. 配置指南内容（静态文案，支持中英）

**被连接设备开启 SSH（三端并列）**：

- mac：系统设置 → 通用 → 共享 → 打开「远程登录」；确保当前用户在允许访问列表。
- Ubuntu：`sudo apt install openssh-server && sudo systemctl enable --now ssh`；若启用 ufw 需 `sudo ufw allow ssh`。
- Windows：设置 → 应用 → 可选功能 → 安装「OpenSSH 服务器」；以管理员 PowerShell 执行 `Start-Service sshd` + `Set-Service -Name sshd -StartupType Automatic`；防火墙放行 22。

**本机连接端用法（按本机 OS）**：

- mac：自带 ssh，终端 `ssh user@ip`；`ssh-keygen` 生成密钥 + `~/.ssh/config` 别名实现免密。
- Ubuntu：`ssh user@ip`（客户端一般已装；缺则 `sudo apt install openssh-client`）。
- Windows：Win10+ 自带 OpenSSH 客户端，PowerShell/cmd 执行 `ssh user@ip`。

## 9. 测试

- **后端单测**（`cargo test` + `cargo clippy`）：`sync/ssh_target.rs` 的 merge 覆盖 6~7 例（见 4.3）。
- **前端**：`npm run build`（tsc 校验 i18n key 与类型）。
- **手测**（需用户协助）：① 配用户名/端口 → 复制命令格式正确；② 两设备间 `trigger_sync` 后配置一致；③ 软删除同步传播。

## 10. 开发方式（遵守项目规则 6/14）

规模超 100 行且前后端都有 → **git worktree 新分支** + **两个 subagent 并行**：

- subagent A（后端，Rust，`sonnet`）：第 4 节全部。
- subagent B（前端，React，`sonnet`）：第 5~8 节全部。
- 我先定第 11 节 IPC 契约，使两者无阻塞并行。完成后我审查 git diff，合并回 master（先切 master 再合并、解冲突、清临时分支）。

## 11. IPC 契约（subagent 并行依据）

### Tauri 命令（camelCase 出参，args camelCase）

- `get_os_info()` → `{ platform: 'mac'|'windows'|'ubuntu', raw: string }`
- `list_ssh_targets()` → `SshTarget[]`
- `upsert_ssh_target({ host: string, username: string, port?: number, label?: string })` → `SshTarget`
- `delete_ssh_target({ host: string })` → `{ ok: boolean }`

### `SshTarget`（前端 camelCase）

`{ host: string, port: number, username: string, label?: string, updatedAt: string }`

### `SshTargetRow`（P2P snake_case，同步用）

`{ host, port, username, label, device_id, vector_clock, updated_at, created_at, deleted }`

### P2P 端点

- `POST /api/ssh-target/sync/pull` body `{summaries:[{host, vector_clock}]}` → `{targets:[SshTargetRow]}`
- `POST /api/ssh-target/sync/push` body `{targets:[SshTargetRow]}` → `{accepted: number}`

## 12. 配套文档更新项（实现完成后）

- 根 `CLAUDE.md`：代码结构图 + 项目概述新增「SSH 配置管理（跨设备同步）」条目。
- `src-tauri/CLAUDE.md`：新增「SSH target 同步已落地行为约定」节（表 + repo + sync + route + commands，对齐既有 M 节体例）。
- `web/CLAUDE.md`：pages/api/i18n 三处清单补 SSH 页。
- `docs/prd.md`：补充 SSH 页功能需求条目（规则 10）。
