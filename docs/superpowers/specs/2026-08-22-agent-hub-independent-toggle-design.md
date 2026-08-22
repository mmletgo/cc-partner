# Agent Hub 分 Agent 独立资产管理（能做到的极限）

> 日期：2026-08-22  
> 状态：待实现  
> 前置调研：[agent-hub-per-agent-toggle-report.md](../../development/agent-hub-per-agent-toggle-report.md)  
> 合同锚点：`adapt-new-agent.md` §3.9–§3.13

## 目标

在 **不改所有者磁盘、不 remap 到另一家 CLI** 的前提下，把 Hub 能单独管的资产都管起来：当前 Agent 开或关，只影响当前 Agent 的加载策略。

「极限」= 官方配置里已经有、或本机文件语义已经成立的开关。不发明 CLI 不认的 Hub 私有黑名单。

## Key Decisions

1. **单独管理 = viewing 写盘。** Enable/Disable 只写当前查看 Agent 的配置或自己的 native 软链。Uninstall 仍可改所有者，但借用行必须用现有「改所有者」文案，且 MCP 借用行不再提供卸载。  
2. **纯文件 viewing 开关与仓库软链同类，不绑 L3 / CLI probe。** Claude plugin 仍 spawn `claude plugin`（继续要探测）。Grok plugin 的 `[plugins]`、以及 Grok/Gemini/Cursor/OpenCode 自身 MCP 的 `enabled`，走 config patch，**零 spawn**。`bypasses_target_cli_gates` 不能把所有 Enable 都放行（会放开 Claude CLI 门闩），因此新增 `(target, kind, action)` 判定 `is_file_only_viewing_toggle`。  
3. **Plugin 不进仓库，MCP 不进仓库。** 跨 Agent MCP 继续用 Pull，不给借用 MCP 做 Enable/Disable。  
4. **借用 Claude MCP（仅 Grok 会列）：Hub 不提供启停/卸载。** 只留「在所有者中打开」。现有 MCP 借用门闩走 owner allowlist，若打开按钮会改 Claude，必须先堵上。  
5. **借用 Skill 卸下只拆当前 Agent 自己的软链。** `loadedViaOtherPath` / 暗淡芯片只读，禁止从 Grok 拆 Claude 的链。  
6. **根级官方门闩算独立管理，但是整根不是逐条。** OpenCode env、Pi `settings.json` `skills` 路径列表可在后续阶段做成 Hub 动作；不做「忽略兼容根里某一条 Skill」的 Hub 私有表。  
7. **不改 support-manifest 把 Grok 的 activatePackage 标成 supported。** 避免暗示已认证 `grok plugin` CLI。短名碰撞问题仍在。

## 非目标

- 从 Grok 调 `claude plugin disable` 或未认证的 `grok plugin disable`  
- 把 Plugin/MCP 迁入 `portable-store`  
- 让 Cursor/Gemini/Pi/OpenCode 列出 Claude plugin cache  
- 为借用 Skill 做逐条 ignore（官方没有）  
- 改变 Claude/Codex 已有的自身 Plugin/MCP 语义  
- 自动给所有 Skill 迁入仓库（可引导，不强制）

## 用户可见合同

行上按钮含义：

| 资产 | 按钮 | 写哪里 |
|------|------|--------|
| Skill/Command 已进仓库 | 附加 / 从此 Agent 卸下 | 当前 Agent native 根软链 |
| Skill/Command 借用且无自己的链 | 无启停；可「在所有者中打开」 | 不写盘 |
| Plugin 自身或 Grok 借用 | 启用 / 禁用 | **当前 Agent** 开关文件 |
| Plugin 卸载 | 卸载 | **所有者** 磁盘 |
| MCP 自身 | 启用 / 禁用 | **当前 Agent** 配置 leaf 的 `enabled` |
| MCP 借用 | 无启停无卸载 | 跳转所有者 |

确认框：Plugin 借用继续用 `borrowedImpactEnablePlugin`（只改当前）。MCP 借用不再打开启停确认框。Skill 借用卸下：仅当 `storeAttached` 且 viewing 自己挂了链。

## 分层

```
UI（capability 驱动，不写新动作 kind）
  → scanner item_capabilities / supports_direct_local_action
    → planner（file-only viewing 跳过 CLI installed/mutation_capability）
      → executor
           Claude plugin: 仍 spawn claude
           Codex plugin/MCP: 已有 toml patch
           Grok plugin: 新 toml [plugins] 数组 patch
           Grok/Gemini/Cursor/OpenCode MCP: 新/复用 leaf enabled patch
           store: 已有软链
```

### `is_file_only_viewing_toggle(target, kind, action) -> bool`

为真当且仅当：

- `action ∈ {Enable, Disable}`  
- 且下列之一：  
  - `target == Grok && kind == Plugin`  
  - `target ∈ {Grok, Gemini, Cursor, OpenCode} && kind == Mcp`

Pi 无 Plugin/MCP 根，不加。Claude/Codex 走原 allowlist + CLI/现有 patch，**不要**标成 file-only（Claude plugin 仍 spawn）。

`supports_direct_local_action`：store 动作保持「仅 Skill/Command」；file-only viewing 为真则 Enable/Disable 为真；其余仍是 Claude|Codex 四类 enable/disable/uninstall。

### Scanner

- Plugin 借用：Enable/Disable 的 `enablement_target` 已是 viewing。Grok 进入 allowlist 后 `can_enable`/`can_disable` 会亮。  
- **MCP 借用：强制 `can_enable = can_disable = can_uninstall = false`**（即使 owner 是 Claude）。reason 保持 `borrowed_runtime_origin`。  
- Skill `can_detach`：仅 `store_attached && !borrowed_store_runtime`（或 borrowed 但 viewing 自己 `storeAttached`）。`loadedViaOtherPath` 不得 `can_detach`。

### Grok Plugin 写盘

文件：`{grokConfigRoot}/config.toml`（项目 scope 用 `{project}/.grok/config.toml`）。

与 `grok_plugin_actual_enabled` 对齐：

- Disable：把 id（优先 marketplace 限定 `plugin_cli_selector`）写入 `plugins.disabled` 数组（去重）；若在 `plugins.enabled` 里则移除。  
- Enable：从 `disabled` 移除；**仅当 native 且 `enabled` 数组非空（白名单模式）** 才把 id 写入 `enabled`。借用包 Enable 不得因白名单模式而被写成「没进 enabled 就不算开」——扫描侧借用未登记视为开，只认 `disabled`。  
- 原子写：`TomlConfigPatcher` + `apply_config_patch_atomically`，patch 整段数组。  
- 禁止 spawn。

### 自身 MCP 写盘

一律只翻 `enabled`，不删 leaf、不挪到 Claude 那种 disabled 快照（避免和 Codex 语义分叉；Claude 保持现有 snapshot 行为）。

| Agent | 文件 | 路径 |
|-------|------|------|
| Grok | `config.toml` | `mcp_servers.{id}.enabled`（与现有 toml 扫描一致） |
| Gemini | `settings.json` | `mcpServers.{id}.enabled` |
| Cursor | `mcp.json` | `mcpServers.{id}.enabled` |
| OpenCode | `opencode.json` / `opencode.jsonc` | `mcpServers.{id}.enabled` |

缺字段视为 true；Disable 时插入 `enabled: false`。JSONC 用已有 `JsoncConfigPatcher`。

借用 MCP 不走这条（见上）。

### 根级门闩（极限的第二层，非逐条）

| Agent | 开关 | Hub 形态 |
|-------|------|----------|
| OpenCode | `OPENCODE_DISABLE_EXTERNAL_SKILLS`、`OPENCODE_DISABLE_CLAUDE_CODE(_SKILLS)` | 不写用户 shell env。若要做：只做 **项目/用户配置里官方支持的落点**（若 CLI 只认环境变量、不认配置文件，则 **不做 Hub 写**，只展示只读状态） |
| Pi | `settings.json` 的 `skills` 路径数组 | 可做：勾选「加载 Claude skills 根 / Codex skills 根」= 点名或移除路径 |

此层不阻塞 Plugin/MCP 主路径。实现前必须再对照当前 CLI：env-only 的门闩 Hub 不能假装能写。

## 测试合同

1. Grok + Claude `enabledPlugins=false` 的借用 plugin：`actual_enabled == true`，`can_disable == true`，preview Disable 的 `change.target == grok`，apply 后只改 Grok toml，`~/.claude/settings.json` 不变。  
2. Grok 白名单 native plugin：Disable 写入 `disabled`；Enable 在白名单模式写回 `enabled`。  
3. Grok 列表里的 Claude MCP：`can_enable`/`can_disable`/`can_uninstall` 全 false。  
4. Grok 自身 MCP Disable 只改 `~/.grok/config.toml`，Claude json 不变。  
5. 借用 Skill `loadedViaOtherPath`：`can_detach == false`。  
6. planner：Grok CLI `mutation_capability=blocked` 时，Grok plugin Disable 的 `blockingReasons` 不含 `MUTATION_BLOCKED` / `CLI_NOT_INSTALLED`。  
7. 同条件下 Claude plugin Disable 仍受 CLI 门闩。

## 风险

- Grok `grok plugin` CLI 与 config.toml 是否双源：Hub 只认 toml（与扫描一致）。若官方后来只认 CLI、不认数组，要停写并改扫描。  
- Gemini `settings.json` 是否所有 server 都认 `enabled`：扫描已读该字段，写回同一字段。  
- OpenCode JSONC 注释：必须走 JsoncConfigPatcher，禁止 serde round-trip 丢注释。

## PR Plan

| PR | 内容 | 依赖 |
|----|------|------|
| PR1 门闩与借用收口 | `is_file_only_viewing_toggle`；MCP 借用禁启停卸载；Skill 借用禁拆源链；planner 跳过 CLI 门闩；测试先红后绿 | 无 |
| PR2 Grok Plugin toml 启停 | 执行器 + 扫描能力亮灯 + 前端无需新 kind（capability 驱动） | PR1 |
| PR3 自身 MCP 启停 | Grok toml → Gemini/Cursor JSONC → OpenCode JSONC | PR1 |
| PR4 文案与根级门闩调研落地 | i18n；adapt-new-agent；可选 Pi/OpenCode 根级（仅当官方有配置落点） | PR2/PR3 |
| PR5 不在本方案 | 借用 MCP 独立黑名单、逐条 ignore Skill、plugin 进仓库 | — |

## Open Questions

无产品二选一。实现 PR3 时若某家 MCP 扫描不到 `enabled`、且 CLI 把缺省当永久开、写 `enabled:false` 无效，则该家 MCP 启停从 PR3 剔除并在 PR 说明里写实测证据，不得改成删 leaf 冒充 Disable。
