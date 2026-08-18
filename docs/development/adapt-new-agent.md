# 接入新 CLI Agent 操作手册

> 给下一次「再适配一种 Agent」用。本文是落地清单：先锁身份，再按面接线，最后用编译器与 grep 清漏网。
>
> 当前已登记：`claude` / `codex` / `opencode` / `grok` / `gemini` / `cursor` / `pi`。`genericTerminal` 只存在于 Runtime，没有 `AgentId` 行。
>
> 相关文档：概念合同 [`docs/superpowers/specs/2026-08-16-agent-capability-catalog-design.md`](../superpowers/specs/2026-08-16-agent-capability-catalog-design.md)；落地计划 [`docs/superpowers/plans/2026-08-16-agent-capability-catalog.md`](../superpowers/plans/2026-08-16-agent-capability-catalog.md)；Hub 写能力门禁 [`docs/development/agent-hub/manifest.md`](agent-hub/manifest.md)。Plugin 启用标记跟 viewing Agent（§3.9，`plugin_enablement.rs`）。Skill/Command/MCP 本机一份在 `<data_dir>/portable-store/`（§3.10，`portable_store/`）。不要把概念 spec 改写成「Cursor 一开始就在」；新身份只追加本手册附录。

## 0. 硬规则（写代码前先接受）

1. **一份身份表**。Hub / Runtime / 会话搜索 / Prompt 历史 / 用量 / headless 只投影 `agent_catalog`，禁止再按功能面复制枚举。
2. **未知 token fail-closed**。parse / decoder 失败，禁止静默映射 Claude。
3. **能做的做实，不能做的仍要露脸**。新身份必须出现在壳层切换器；做不到的面用 scan-only / blocked / residual / unavailable / 缺席，禁止从 UI 藏掉。
4. **无 L3 evidence 不写原生文件**。`support-manifest.json` 默认 `renderInstruction` / portable 写 / activate / deactivate = `blocked`。扫描可以 `readOnly`。没有已认证 executor 时，**禁止**把 Enable/Disable 映射到另一家的 CLI（例如在 Grok 列表里跑 `claude plugin disable`）。
5. **Plugin 开关跟当前查看的 Agent，不跟所有者。** Claude `enabledPlugins=false` 不得让 Codex / Grok / OpenCode / Gemini / Cursor / Pi 的同一包显示为已关。Enable/Disable 只写 viewing 标记；Plugin Uninstall 仍改所有者磁盘。详见 §3.9。
6. **Skill / Command / MCP 本机一份在 portable-store，不在 `~/.agents`。** 附加只在该 Agent **自己的 native 根**建软链（MCP 则投影 leaf）；卸下只拆 viewing 的链/leaf。Grok 仍扫 Claude 路径时，只提示「仍被其他路径加载」，禁止为列表干净去改 Claude。store 软链禁用 **不得 MOVE 真树**。无 L3 不得给新 Agent apply attach/detach/migrate/destroy。详见 §3.10。
7. **不碰这些面**：可切换 LAN 模式、鉴权矩阵、把 peer 称为已认证设备、自动安装 CLI、读取 API key、把 `cc-switch` / Provider Manager 并进身份目录、为新 CLI 伪造 Claude status 文件或 OpenCode runtime bridge。
8. **可执行名 ≠ 产品名**。只启动官方 CLI（Cursor 是 `agent`），禁止拉起 GUI。

## 1. 先锁身份（未锁完禁止开写）

填完这张表再改代码。空格允许，但必须写明「缺席 / unavailable / 本轮不做」而不是留白。

| 决策 | 写什么 | 例子（Cursor CLI） |
|------|--------|-------------------|
| wire `AgentId` | 稳定小写 token，禁止别名顶替另一行 | `cursor`（不要叫 `agent`） |
| 显示名 | 中英 UI 共用的产品名 | Cursor CLI |
| CLI 可执行文件 | PATH 上的二进制；probe `--version` | `agent` |
| 配置根 / 覆盖 env | 用户级 home | `~/.cursor`，`CURSOR_HOME` |
| 是否读仓库 `AGENTS.md` | 决定公共槽能不能再写一份 | 读 → **公共槽不物化**（跟 Grok） |
| 适配 / 独有槽落点 | 单一路径，禁止同一槽双写 | `.cursor/rules/cc-partner.{adapted,exclusive}.mdc` |
| 特殊文件格式 | 扩展名、frontmatter、忽略规则 | 必须 `.mdc` + `alwaysApply` YAML；禁止把用户正文塞进 YAML |
| Claude 兼容目录 | 是否会扫 `~/.claude` | 会扫 → **禁止当本身份 native 输出** |
| Portable Skill/Command | 目录约定 | `.cursor/skills`、`.cursor/commands` |
| MCP | 文件 + 键名 + TOML/JSONC | `.cursor/mcp.json` 的 `mcpServers`（JSONC） |
| Runtime provider | camelCase，带 `Visible` 后缀 | `cursorCliVisible` |
| 启动 / resume | argv + stdin | 空 args + prompt stdin；`agent --resume {id}` |
| 完成合同 | 无稳定 sentinel 就 Manual | `Manual` + `ResumeTerminalPolicy::Fresh` |
| 会话布局 | 有合同才扫盘 | 未固化 → tab 在、结果 `unavailable` |
| 用量抽取 | 有稳定字段才实现 | `has_usage: true`，extract 返回 `None` |
| Prompt 历史 | 有用户输入文件才加 collector | `history_source` 可登记；无 collector 则筛选为空 |
| Headless / 优化器 | catalog 位 ≠ 设置里可选 | `has_headless: true`，优化器仍只开 claude+grok |
| Plugin / marketplace | 原生 plugin 根、**本 Agent 的开关文件**、是否真会加载其他 Agent 的 registry | 只扫自己的目录；**不要**抄 Grok 去列 Claude `installed_plugins.json`，除非该 CLI 运行时确实加载。跨 Agent 翻译仍 residual |
| portable-store 挂载点 | native skills / commands / MCP 配置文件；是否扫其他 Agent 根 | Skill=`~/.cursor/skills`；Command=`~/.cursor/commands`；MCP=`mcp.json` `mcpServers`。会扫 Claude 则必须处理 `loadedViaOtherPath`。无 L3 → apply blocked |

选模板（只抄结构，不抄路径）：

| 模板 | 何时用 | 参考实现 |
|------|--------|----------|
| **Grok 型** | 也读 `AGENTS.md` / `CLAUDE.md`，专属语义进自己的 rules | `targets/grok.rs` + `grok_build.rs` |
| **Gemini 型** | 不读 `AGENTS.md`，有自己的主指令文件 | `targets/gemini.rs` + `gemini_cli.rs` |
| **Claude / Codex 型** | 该 CLI 就是该文件的权威所有者 | 不要给后来者再抢 `CLAUDE.md` / `AGENTS.md` |

Cursor 走 **Grok 型**：公共槽复用已有 `AGENTS.md`，专属写 `.cursor/rules/*.mdc`。

## 2. 推荐落地顺序

按这个顺序改，编译器才能尽早抓住非穷尽 match。每步都把「前一端 identity」对齐后再进下一步。

1. **身份目录**（Rust 权威 + 前端镜像）
2. **Hub 枚举与路径**（`AgentTarget`、`TargetHomes`、support-manifest）
3. **Hub AssetAdapter**（新 `targets/<id>.rs`，单测锁「不写 AGENTS.md / 不写 ~/.claude」）
4. **Runtime AgentAdapter**（新 `orchestrator/agent_adapter/<id>.rs` + registry）
5. **穷尽 match / 列表**（Hub 投影、portable、`plugin_enablement`、`portable_store`、packages、session、usage）
6. **前端类型 / decoder / i18n / 列表 helper**
7. **验证**（§6）

不要先做 UI 再补 catalog。壳层列表必须读 `allHubTargets()` / `allSessionSources()` / `allHistorySources()`。

## 3. 分面改哪些文件

下列路径以 2026-08 代码树为准。新增身份时 **每个 `match target` / `match provider` / `Record<AgentTarget, …>` 都要有新臂**；能改成读 catalog 的列表就不要再手写字面量。

### 3.1 身份目录

| 位置 | 做什么 |
|------|--------|
| `src-tauri/src/agent_catalog/mod.rs` | 加 `AgentId` variant + `IDENTITIES` 行；更新 `catalog_registers_*` 单测长度 |
| `web/src/lib/agentCatalog.ts` | 同步一行；`headlessOptimizerProviders()` 仍白名单已实现的优化器，不要只看 `hasHeadless` |
| `web/src/lib/agentCatalog.test.ts` | hub / session / history 列表断言 |

投影缺省语义：

- `hubTarget: None` → 不进 Agent Hub / 跨 Agent
- `runtimeProvider: None` → 不进编排器 / 可见 Runner
- `sessionSource: None` → ⌘K 不出现该 tab
- `historySource: None` → Prompt 历史筛选不出现；有 source 无 collector 则 tab 在、列表空
- `has_usage: false` → Token 统计不抽该 provider
- `has_headless: true` 只表示「登记过」；设置页可选集另滤

### 3.2 Hub：枚举、路径、manifest

| 位置 | 做什么 |
|------|--------|
| `src-tauri/src/agent_hub/models.rs` | `AgentTarget` + `ALL` 长度 + `as_str` / `parse` / `executable_name` / `native_instruction_file_name` |
| `web/src/lib/types/agentHub.ts` | `AgentTarget` 联合 |
| `web/src/lib/schemas/agentHub.ts` | `agentTargetDecoder` |
| `src-tauri/src/agent_hub/targets/paths.rs` | env 白名单、`TargetHomes` 字段、`resolve_*_home`、`default_user_instruction_path`；单测覆盖覆盖 env |
| `src-tauri/src/agent_hub/support/support-manifest.json` | 新 `targets[]`；写能力全 `blocked`，扫描 `readOnly`；`commandNames` 用真实 CLI；`activatePackage` / `deactivatePackage` 无 L3 保持 `blocked` |
| `src-tauri/src/agent_hub/support/runtime-discovery.json` | 只登记该 CLI **真实会加载**的根。`pluginRegistry` / `pluginMarketplace` 今天仅 Grok 指向 Claude cache；没有运行时证据就不要加 |
| `src-tauri/src/agent_hub/support/manifest.rs` | 若有名字列表单测，补新 token |

### 3.3 Hub：AssetAdapter

复制最接近的模板到 `src-tauri/src/agent_hub/targets/<id>.rs`，然后：

1. `targets/mod.rs`：`pub mod`、`pub use`、`probe_target` match
2. 接到 `service.rs` 的 adapter vec、`projection_ops.rs`、`instructions/compiler.rs`、`user_instructions/inventory.rs`、`portable_inventory/{scanner,plugin_enablement}.rs`、`portable_store/`、`portable_actions/{planner,targets}`、`packages/{builder,adoption,activator}`、`plugins/decompose.rs`、`replication/pull.rs`、`cross_agent.rs`、`tests/agent_hub_cli_contract.rs`
3. 单测至少覆盖：公共槽不写不该写的共享文件；Claude 兼容目录不是 native；受管文件名固定；**plugin `actualEnabled` 不继承 Claude `enabledPlugins`**（§3.9）；**store 软链只跟随 `portable-store/`，逃逸拒绝**（§3.10）

指令槽经验：

- 后来者若也读 `AGENTS.md`，**不要再写一份**（Codex/OpenCode 已占用）
- Cursor 类 rules 引擎若忽略纯 `.md`，必须用它真正加载的扩展名，并在 render 时包一层**静态** YAML（description 禁止插入用户正文，已有 `---` 则跳过）

### 3.4 Runtime

| 位置 | 做什么 |
|------|--------|
| `src-tauri/src/orchestrator/agent_adapter/types.rs` | `AgentProviderId` + parse/as_str + completion 默认 + wire roundtrip 测试 |
| 新 `…/agent_adapter/<id>.rs` | 抄 `grok_build.rs` / `gemini_cli.rs`：2s probe、空 args、stdin prompt、Fresh resume、Manual |
| `mod.rs` + `registry.rs` `with_defaults()` | 注册 adapter |
| `web/src/lib/types/orchestrator.ts` + `schemas/orchestrator.ts` | provider 联合 / decoder |
| `web/src/lib/agentAdapterPresentation.ts` | Settings / Workbench 标签 key |
| i18n `settings.json` / `orchestrator.json` / `workbench.json` 的 `providers.*` 与 `automation.provider.*` | 中英同时加 |

未安装：catalog 行仍在，`availability = unavailable`，`reason_code = provider_unavailable`。

### 3.5 会话搜索

| 位置 | 做什么 |
|------|--------|
| `src-tauri/src/workbench/agent_session_search.rs` | `AgentSessionSource` parse/as_str；`search_*`；`build_resume_command`；`check_agent_cli_available` |
| `src-tauri/src/commands/workbench/sessions.rs` | **三处** match：search / preview / resume；错误文案里的允许列表 |
| `web/src/components/domain/WorkbenchSessionSearch/WorkbenchSessionSearch.tsx` | tab 来自 `allSessionSources()`；**每个 source 都要有 typed i18n 标签**，禁止 `codex/opencode/else→Claude` 这种掉进 Claude 的三元式 |
| `web/src/i18n/locales/{en,zh}/workbench.json` | `sessionSearch.agents.<id>` |

磁盘布局没有官方合同就返回 `diagnostics.status = unavailable`，不要猜路径、不要误扫 Claude jsonl。

### 3.6 用量

`src-tauri/src/workbench/agent_runtime/agent_usage.rs`：

- `is_usage_extractable_provider` 加上 runtime id **和** catalog 短码
- `extract_provider_usage` / `locate_*` 无证据时走 `_ => None`
- **禁止把缺失写成 0**；UI 显示「未提供」

### 3.7 Prompt 历史

有稳定用户输入文件再加：

- `src-tauri/src/cc/sources/<id>.rs` + `models.rs` 的 `SOURCE_*` + `collector.rs::scan_once`
- 主键带源前缀，避免与 Claude `{session}:{uuid}` 碰撞
- 前端 `CcHistorySource`、`CcHistory.tsx` 的 source 标签、`ccHistory.json` 的 `sourceX`

没有 collector 也可以先把 `historySource` 登记上（筛选会出现空列表）；若不想空 tab，把 catalog 的 `historySource` 留 `null`。

### 3.8 前端列表与 i18n（最容易漏）

UI 列表必须读 catalog。仍会手写、必须同步的地方：

| 类型 | 位置 |
|------|------|
| `Record<AgentTarget, …>` 字面量 | `useUserInstructionManager.ts`、`useInstructionThreePaneController.ts` 及它们的测试、`agentHub.peerContext.test.ts` |
| 跨 Agent 目的地 | `crossAgentPresentation.ts` 测试里的期望数组 |
| portable pull 标签 | `portablePullPresentation.ts` 的 `sameAs<Id>` + `agentHub.json` |
| Agent Hub 文案 | `agentHub.json`：`targets`、`targetFilter`、三槽 placeholder、pane 名；plugin 借用 `borrowedHintPlugin` / `borrowedImpactEnablePlugin` / `borrowedImpactDisablePlugin`；store `storeAttachHint` / `storeDetachHint` / `storeMigrateHint` / `storeDestroyHint` / `storeStillLoadedVia`；`borrowedFrom.portableStore` |
| decoder | `schemas/agentHub.ts`、`schemas/orchestrator.ts` |

`Record<AgentTarget, T>` 不会随 catalog 变长而自动补键，`tsc -b` 会在这里抓住漏网。新增身份后用 catalog 生成空对象：

```ts
Object.fromEntries(allHubTargets().map((t) => [t, 'unmanaged'])) as Record<AgentTarget, …>
```

测试里的期望对象也要加新键。

### 3.9 Portable plugin：启用标记跟 viewing Agent

Skill / Command / MCP 一旦进入 portable-store，启停是 **viewing 附加**（§3.10）。未进 store 的 native 项，Claude/Codex 仍可能 MOVE 所有者目录。Plugin **始终不是 store**：每个 Hub target 有自己的开关，关掉 Claude 不等于关掉 Grok。

权威实现：`src-tauri/src/agent_hub/portable_inventory/plugin_enablement.rs`。`ViewingPluginEnablement::load(target, …)` **只读当前查看 Agent 的配置**；`plugin_actual_enabled` 必须穷尽 `AgentTarget`。禁止在 scanner 里再写一套 `if Claude / if Codex / else 目录存在即开`。

| Viewing target | 开关文件 | native 未登记 | borrowed 未登记 |
|----------------|----------|---------------|-----------------|
| Claude | `~/.claude/settings.json` → `enabledPlugins`（优先完整 `id@marketplace`） | 已安装且表中无键 → **开** | 查看 Claude 时走 Claude 表 |
| Codex | `config.toml` → `[plugins."id@market"] enabled` | 表非空且未登记 → **关** + `codex_plugin_not_in_config` | **开**（不得把 native 白名单套到借用包） |
| Grok | `config.toml` → `[plugins] enabled = […]` / `disabled = […]` | `enabled` 非空则当白名单 | 只认 `disabled`；**不要**把 Claude `enabledPlugins` 或 Grok `enabled` 白名单当成借用包已关 |
| OpenCode / Gemini / Cursor / Pi | 无独立 Hub 可读开关 | 目录存在 → **开** | 永不读 Claude / Codex / Grok 的开关文件 |

写盘分流（`targets/portable.rs::mutation_target_for_action`）：

- Plugin **Enable / Disable** → **viewing** target（改当前 Agent 的标记）
- Plugin **Uninstall** → **owner**
- 未进 store 的 Skill / Command / MCP 启停卸载 → **owner**（`SharedAgents` → Codex）
- 已进 store 的 Skill / Command / MCP → **viewing** 的 native 根（建/拆链或 leaf）；`destroyStore` 才动真树

Direct-local allowlist（`portable_actions/targets/mod.rs::supports_direct_local_action`）目前只有 **Claude | Codex**。Grok / OpenCode / Gemini / Cursor / Pi 的 `activatePackage` / `deactivatePackage` 在 support-manifest 里仍是 `blocked`：列表里不得出现可点的 Enable/Disable，更不得 remap 到 `claude plugin …`。Grok 短名 `grok plugin disable superpowers` 会碰到 native 安装与 Claude cache 同名，未认证前禁止从 Hub 去调。

盘点范围：

- 只有 `runtime-discovery.json` 为该 target 登记了 `pluginRegistry` / `pluginMarketplace`，并且该 CLI **运行时确实加载**那些包，才允许在它的库存里列出 Claude cache。今天 **仅 Grok**。
- Codex / OpenCode / Cursor / Gemini / Pi **不要**为了对称去扫 `~/.claude/plugins/installed_plugins.json`。列出来等于谎称它们会加载 Claude plugin 包。
- 不要把 Grok marketplace / Gemini extensions / Cursor plugins 翻译成 Claude plugin。

前端文案：plugin 借用横幅用 `borrowedHintPlugin`；确认框 Enable/Disable 用 `borrowedImpactEnablePlugin` / `borrowedImpactDisablePlugin`（只改当前 Agent）；卸载仍用所有者影响提示。Plugin **不出现**迁入便携仓库主按钮。

接入新身份时 plugin 最低测试：

- `plugin_enablement`：空 viewing store 不得继承另一家；borrowed 忽略 Claude-off
- scanner：该 target 原生 plugin + Claude `enabledPlugins=false` → `actual_enabled == Some(true)`（除非该 target 自己的开关文件把它关掉）
- planner：从该 target 预览 borrowed plugin Disable 时 `change.target ==` viewing，不是 Claude

### 3.10 Portable store：本机一份，按 Agent 附加

四类资产**并不都通用**。接入新身份时按合同选挂载方式，禁止为对称去软链 Plugin 包。

权威实现：`src-tauri/src/agent_hub/portable_store/`。真树只在 `<data_dir>/portable-store/`（跟 `CC_PARTNER_DATA_DIR`），**不是** `~/.agents`（Claude / Grok 不把它当技能库）。`ownedBy: portableStore`。scanner 只跟随 canonicalize 落在 store 内的软链；逃逸 fail-closed。

| 资产 | 真树 | 附加到该 Agent | 从此 Agent 卸下 | 本机彻底删除 |
|------|------|----------------|-----------------|--------------|
| Skill | `portable-store/skills/<id>/` | native skills 根上的**目录软链** | 只拆 viewing 这条链 | 删真树 + 清 Claude/Codex 链 |
| Command | `portable-store/commands/<id>.md` | native commands 根上的**文件软链**；无 command 根（如 Pi）→ 缺席 | 同上 | 同上 |
| MCP | `portable-store/mcp/<id>.json`（0600，含凭据） | **不是软链**：投影到 viewing 的 config leaf（JSON `mcpServers` / TOML `mcp_servers`） | 只改 viewing leaf | 删 store JSON + 清 Claude `~/.claude.json` 与 Codex `config.toml` |
| Plugin | 不进仓库 | 仍走 §3.9 viewing 开关；不翻译 marketplace | Enable/Disable 只改 viewing | Uninstall 仍改所有者磁盘 |

未迁入 store 的 native Skill/Command：Claude/Codex 仍可能 **MOVE** 到 hub disabled（旧语义）。**已经是 store 软链**时，Disable/Detach/Uninstall-from-agent 必须 `unlink`，禁止 `portable_set_tree_enabled` 把真树 rename 进 `claude-assets/disabled`。

一键迁移：该 Agent 盘点里非软链 native Skill/Command → move 进 store → 原处放回软链。MCP 把 leaf 复制进 store（0600）并记下 attachment。同名不同 hash → 阻断。Plugin 不迁移。

Grok（以及任何会扫 Claude 根的后来者）：

- 卸下只拆**自己的 native 根**。
- 同一 `storeId` 若 Claude 仍附加，本 Agent 库存去重后保留 `store.loadedViaOtherPath` + `loadedViaTarget=claude` + warning `store_loaded_via_other_path`。文案 `storeStillLoadedVia`。
- **不得**为了本列表干净去拆 Claude 链或改 Claude MCP。

写盘门禁：无 L3 时 **只有 Claude / Codex** 能 apply attach / detach / migrateToStore / destroyStore（复用 `supports_direct_local_action`，不要为此新增 Supported write-side `TargetCapability`）。新身份默认 inventory + preview，apply blocked。Windows 用 junction/symlink，禁止静默 copy 成第二份安装。

接入新身份时 store 最低测试：

- scanner：store 软链 Skill 的 content hash 跟随真树；指向 `/etc` 的逃逸链拒绝
- 若该 CLI 扫 Claude skills：Claude 已附加、本 Agent native 无链 → `loadedViaOtherPath`，且 Claude 链未改
- planner/apply：无 L3 时 attach/detach/migrate/destroy 不得写该 Agent 原生目录
- MCP（仅 Claude/Codex 已实现）：detach viewing 只改该配置文件

## 4. 编译器抓不到的漏网（必须 grep）

`cargo check` 能抓住 Rust 穷尽 match。下面这些 **不会** 报非穷尽，接入后要搜一遍旧的「最后一家」：

```bash
# 仍写死五家 / 三家的列表
rg -n "claude.*codex.*opencode.*grok.*gemini[^\"]" src-tauri web/src

# 前端仍手写 unmanaged 五行
rg -n "gemini: 'unmanaged'" web/src

# 会话搜索仍把未知 source 显示成 Claude
rg -n "sessionSearch.agents.opencode" -A 6 web/src/components/domain/WorkbenchSessionSearch

# Hub 文案 / 筛选
rg -n "targetFilter" web/src/i18n/locales/en/agentHub.json

# support-manifest 是否少一行
rg -n '"target": "' src-tauri/src/agent_hub/support/support-manifest.json

# plugin 开关是否漏新臂 / 是否误扫 Claude registry
rg -n "match enablement.target" src-tauri/src/agent_hub/portable_inventory/plugin_enablement.rs
rg -n '"kind": "pluginRegistry"' src-tauri/src/agent_hub/support/runtime-discovery.json

# portable-store 是否漏新 Agent 挂载 / 是否把 ~/.agents 当 store
rg -n "portable_store_root|PortableAssetOwner::PortableStore" src-tauri/src/agent_hub
rg -n "ownedBy: 'portableStore'|loadedViaOtherPath" web/src
```

文档：根 `AGENTS.md` 产品一句、`src-tauri/AGENTS.md` 的 `targets/` 文件名单。不要在文档里宣称 L3 真机写盘已认证。不要写「关掉 Claude plugin 等于关掉所有 Agent」。不要写「`~/.agents` 就是全 Agent 统一库」。

## 5. 能力状态怎么填

| 状态 | 用在哪 | 用户应看到 |
|------|--------|------------|
| 支持 | 有合同且已实现 | 正常列表 / 可启动 |
| scan-only / blocked | Hub 写无 L3 | 能扫、不能保存到原生文件；稳定错误码 |
| unavailable | CLI 未装或目录未知 | tab 在，诊断文案，不是空白崩溃 |
| residual | 跨 Agent 不可翻译的 Plugin/Hook | 计划项标 residual，不标成功 |
| 缺席 | 该面根本不投影 | catalog 对应字段 `None` / `false` |

打开原生写入的条件：quality-matrix 里有对应 `L3-…` evidence，并且 `support-manifest` 的 `capabilityEvidenceIds` 指向它。未认证保持 blocked。

## 6. 验证

```bash
# Rust：身份 + 新 adapter + 路径覆盖 env
cd src-tauri
cargo fmt --all
cargo check --locked --all-targets
cargo test --locked --lib -- agent_catalog::
cargo test --locked --lib -- targets::<id>::
cargo test --locked --lib -- plugin_enablement
cargo test --locked --lib -- agents_without_plugin_flags
cargo test --locked --lib -- portable_store
cargo test --locked --lib -- grok_unattached_store

# 前端
cd web
npm run check:i18n
npx --no-install tsc -b --pretty false
npx --no-install vitest run src/lib/agentCatalog.test.ts \
  src/pages/AgentHub/crossAgent/crossAgentPresentation.test.ts \
  src/pages/AgentHub/portableAssets/portablePullPresentation.test.ts
```

本机若已有 `tauri dev` 占用 `src-tauri/target`，用独立目录避免锁：

```bash
CARGO_TARGET_DIR=/tmp/cc-partner-agent-check cargo check --locked --lib --tests
```

`tsc -b` 会编进测试文件：任何 `Record<AgentTarget, …>` 测试夹具漏键都会失败，这是预期门禁。

## 7. 实例：Cursor CLI（2026-08-17）

当时按 Grok 型接入，作为第六身份。决策与结果：

| 项 | 取值 |
|----|------|
| wire / 显示名 / CLI | `cursor` / Cursor CLI / `agent` |
| home | `~/.cursor`，`CURSOR_HOME` |
| 公共槽 | 不写 `AGENTS.md` |
| 适配 / 独有 | `.cursor/rules/cc-partner.adapted.mdc` 与 `cc-partner.exclusive.mdc`，静态 `alwaysApply` frontmatter |
| 扫描 | 项目 `AGENTS.md` NativePrimary（只读）；`CLAUDE.md` / `.cursorrules` Fallback；`.cursor/rules/*.mdc`；绝不把 `~/.claude` 当 Cursor native |
| Portable | `.cursor/skills`、`.cursor/commands`、`mcp.json` → `mcpServers`（JSONC）；无独立 plugin 开关，不得继承 Claude `enabledPlugins`；portable-store 可盘点，apply attach 仍 blocked（无 L3） |
| Runtime | `cursorCliVisible`；stdin prompt；`agent --resume {id}`；Manual；Fresh |
| 会话搜索 | 已登记；v1 `unavailable`（布局未认证） |
| 用量 | 已登记；extract = `None` |
| 历史 collector | 未做；`historySource: 'cursor'` 筛选可为空 |
| 优化器 | catalog `hasHeadless`，设置页仍仅 claude+grok |
| Hub 原生写 | support-manifest 全 blocked（同 Grok/Gemini） |
| cc-switch | 未扩展 |

落地时漏过、后来靠 `tsc` / 穷尽 match 抓住的点，写进 §4 就是为了下次不要再漏：

- `commands/workbench/sessions.rs` 三处 `AgentSessionSource` match
- `Record<AgentTarget, UserInstructionTargetSelection>` 测试夹具
- 会话搜索 tab 把 grok/gemini/cursor 显示成 Claude
- Prompt 历史 `sourceCursor` 文案

## 8. 实例：Pi（2026-08-17）

当时按 Grok 型接入，作为第七身份。决策与结果：

| 项 | 取值 |
|----|------|
| wire / 显示名 / CLI | `pi` / Pi / `pi` |
| home | `~/.pi/agent`（官方目录；文档无覆盖 env，未臆造 `PI_HOME`） |
| 公共槽 | 不写 `AGENTS.md` |
| 适配 / 独有 | `.pi/cc-partner.adapted.md` 与 `cc-partner.exclusive.md`（Pi 无官方 rules 引擎；单一落点，不双写 `APPEND_SYSTEM.md`） |
| 扫描 | 项目 `AGENTS.md` NativePrimary（只读）；`CLAUDE.md` / `AGENTS.override.md` / `.pi/SYSTEM.md`；绝不把 `~/.claude` 当 Pi native |
| Portable | `.pi/skills`、`~/.pi/agent/skills`；无内建 MCP，不伪造 `mcp.json`；无 command 根（store Command 缺席）；无独立 plugin 开关，不得继承 Claude `enabledPlugins`；store apply blocked |
| Runtime | `piVisible`；stdin prompt；`pi --session {id}`；Manual；Fresh |
| 会话搜索 | 已登记；v1 `unavailable`（JSONL 布局未认证） |
| 用量 | 已登记；extract = `None` |
| 历史 collector | 未做；`historySource: 'pi'` 筛选可为空 |
| 优化器 | catalog `hasHeadless`，设置页仍仅 claude+grok |
| Hub 原生写 | support-manifest 全 blocked（同 Grok/Gemini/Cursor） |
| cc-switch | 未扩展 |

## 9. 明确不要做的

- 不要为了「看起来对称」去猜 session 目录或 usage JSON 字段
- 不要把 GUI 二进制（如 `cursor`）当 CLI
- 不要把新 CLI 的 Claude 兼容扫描根再写一套副本
- 不要把 Plugin marketplace 翻译成另一家的 plugin，也不要把 Plugin 迁入 portable-store
- 不要在 `runtime-discovery.json` 里给不会加载 Claude plugin 的 Agent 加 `pluginRegistry`
- 不要把 Claude `enabledPlugins`（或 Codex/Grok 白名单）当成其他 Agent 的 `actualEnabled`
- 不要在无 L3 / 无本 target executor 时，把 Enable/Disable 映射到所有者 CLI（例如 Grok 列表调用 `claude plugin disable`）
- 不要把 native plugin 白名单套到借用包（缺席 ≠ 已关）
- 不要把 `~/.agents` 当成 Claude / Grok 的统一技能库；store 只在 `<data_dir>/portable-store/`
- 不要跟随任意 symlink，只跟随目标在 `portable-store/` 内的链
- 不要为「从会扫 Claude 的 Agent 卸下」去改 Claude 磁盘
- 不要把 store 软链 Disable 做成 MOVE 真树进 disabled
- 不要静默 copy 成第二份安装来绕过 Windows symlink 权限
- 不要在同一槽写两个落点「以防万一」
- 不要把 `has_headless` 直接接到 Prompt 优化下拉（Gemini/Cursor 都登记了，但优化器未开放）
