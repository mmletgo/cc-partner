# 接入新 CLI Agent 操作手册

> 给下一次「再适配一种 Agent」用。本文是落地清单：先锁身份，再按面接线，最后用编译器与 grep 清漏网。
>
> 当前已登记：`claude` / `codex` / `opencode` / `grok` / `gemini` / `cursor` / `pi`。`genericTerminal` 只存在于 Runtime，没有 `AgentId` 行。
>
> 相关文档：概念合同 [`docs/superpowers/specs/2026-08-16-agent-capability-catalog-design.md`](../superpowers/specs/2026-08-16-agent-capability-catalog-design.md)；落地计划 [`docs/superpowers/plans/2026-08-16-agent-capability-catalog.md`](../superpowers/plans/2026-08-16-agent-capability-catalog.md)；Hub 写能力门禁 [`docs/development/agent-hub/manifest.md`](agent-hub/manifest.md)。Plugin 启用标记跟 viewing Agent（§3.9，`plugin_enablement.rs`）。Skill/Command 本机一份在 `<data_dir>/portable-store/`（§3.10，`portable_store/`）；MCP 仍是各家配置 native leaf，跨 Agent 走已有 Pull。漂移项「确认当前版本」只改 Hub 账本（§3.11）。逃逸软链「恢复为仓库资产」把源树复制进 store、在 native 路径挂正规软链，不删源树（§3.12）。用户发起的 user-mirror 可写该身份用户级白名单文件与 MCP leaf / viewing Disable，不 spawn 未认证 CLI；新身份必须进入 `all_hub_targets()`，否则镜像缺席要显式失败而不是跳过。不要把概念 spec 改写成「Cursor 一开始就在」；新身份只追加本手册附录。

## 0. 硬规则（写代码前先接受）

1. **一份身份表**。Hub / Runtime / 会话搜索 / Prompt 历史 / 用量 / headless 只投影 `agent_catalog`，禁止再按功能面复制枚举。
2. **未知 token fail-closed**。parse / decoder 失败，禁止静默映射 Claude。
3. **能做的做实，不能做的仍要露脸**。新身份必须出现在壳层切换器；做不到的面用 scan-only / blocked / residual / unavailable / 缺席，禁止从 UI 藏掉。
4. **无 L3 evidence 不写原生文件**。`support-manifest.json` 默认 `renderInstruction` / portable 写 / activate / deactivate = `blocked`。扫描可以 `readOnly`。没有已认证 executor 时，**禁止**把 Enable/Disable 映射到另一家的 CLI（例如在 Grok 列表里跑 `claude plugin disable`）。**例外**：§3.11「确认当前版本」只写 Hub SQLite 账本；§3.12「恢复为仓库资产」把逃逸目标复制进 store 并在 native 路径挂正规软链（不 spawn CLI、不 remap、不删源树）；用户发起的 user-mirror（`agent-hub.user-mirror.v1`）允许写该身份用户级白名单原生提示词文件、MCP 配置 leaf 与 Plugin viewing Disable，不 spawn 未认证 CLI、不 remap 到另一家 executor。三者无 L3 `activatePackage` 也必须能 apply。Grok/Codex Plugin 的 viewing 启停，以及 Grok/Gemini/Cursor/OpenCode/Codex **自身** MCP 的 `enabled`，是配置文件 patch：不 spawn CLI，不要求 L3 `activatePackage`。Codex Plugin **卸载**仍要 DeactivatePackage。探测 CLI 时 PATH 必须覆盖 nvm/fnm/volta/asdf 当前 bin，并用同一 PATH 跑 `--version`（`#!/usr/bin/env node` 包装脚本才能找到 node）。借用 MCP 不得 Enable/Disable/Uninstall。
5. **Plugin 开关跟当前查看的 Agent，不跟所有者。** Claude `enabledPlugins=false` 不得让 Codex / Grok / OpenCode / Gemini / Cursor / Pi 的同一包显示为已关。Enable/Disable 只写 viewing 标记；Plugin Uninstall 仍改所有者磁盘。详见 §3.9。
6. **Skill / Command 本机一份在 portable-store，不在 `~/.agents`。** MCP **不进仓库**：启停/卸载走当前（或 owner）配置 leaf，跨 Agent 用已有 Pull，不要 `migrateToStore` / `attach` / `detach` / `destroyStore`。附加只在该 Agent **自己的 native 根**建软链；卸下只拆 viewing 的链。会扫 Claude / `.agents` / Codex 路径时，只提示「仍被其他路径加载」，禁止为列表干净去改所有者磁盘。store 软链禁用 **不得 MOVE 真树**。无 L3 不得给新 Agent apply attach/detach/migrate/destroy。借用芯片必须跟该 CLI **官方会加载的目录**走，禁止抄别家扫描表。详见 §3.10、§3.13。
7. **漂移「确认当前版本」只改 Hub 账本。** Hub 投影/store 物化过的 Plugin/MCP/已附加 Skill·Command，磁盘哈希分叉后用户可以把当前文件记为一致基准。Hub 从未写过的独立 Skill/Command 刷新库存即跟随磁盘，不必确认。禁止把这条动作绑到 `supports_direct_local_action`、CLI mutation 门禁或另一家 executor。按钮文案是「确认当前版本」/ `Confirm current version`，不要写成「接受磁盘」或「重记哈希」。详见 §3.11。
8. **逃逸软链「恢复为仓库资产」是布局修复：复制进仓库并挂正规软链。** `store_symlink_escape` / `source_blocked` 的 Skill/Command 必须能在已装备列表以问题行出现，并一键把逃逸目标复制进 `<data_dir>/portable-store/`，再把 native 路径换成指向仓库的软链。禁止 remap 到另一家 CLI，禁止绑 `supports_direct_local_action`，禁止删除 `~/.agents` 源。不是 `migrateToStore`（那条是把 native 真树 rename 进仓库）。文案「恢复为仓库资产」/ `Restore into store`。详见 §3.12。
9. **不碰这些面**：可切换 LAN 模式、鉴权矩阵、把 peer 称为已认证设备、自动安装 CLI、读取 API key、把 `cc-switch` / Provider Manager 并进身份目录、为新 CLI 伪造 Claude status 文件或 OpenCode runtime bridge。
10. **可执行名 ≠ 产品名**。只启动官方 CLI（Cursor 是 `agent`），禁止拉起 GUI。
11. **新身份必须进入 `all_hub_targets()`。** 用户级镜像一次处理 catalog 全部 Hub Agent；缺席不得静默跳过，必须显式失败（unavailable / fail-closed）。禁止只镜像 Claude/Codex/OpenCode 子集而把后来者当不存在。见 [`2026-08-23-agent-hub-user-mirror-design.md`](../superpowers/specs/2026-08-23-agent-hub-user-mirror-design.md)。

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
| 运行时借用根 | 该 CLI **官方文档写明会加载**的他人目录；禁止为对称抄 Cursor 的 Claude+Codex+`.agents` 套餐 | Cursor：Claude + **Codex** + `.agents`；Grok：Claude + 用户级 `.agents`（**不扫** `.codex`）；Gemini：仅 `.agents` |
| 借用门控 | 环境变量 / settings 必须与官方开关同语义 | OpenCode：`OPENCODE_DISABLE_CLAUDE_CODE(_SKILLS)` 只关 `.claude`；`.agents` 只认 `OPENCODE_DISABLE_EXTERNAL_SKILLS`。Pi：Claude/Codex 目录仅 settings `skills` **点名该条路径** |
| Portable Skill/Command | 目录约定 | `.cursor/skills`、`.cursor/commands` |
| MCP | 文件 + 键名 + TOML/JSONC | `.cursor/mcp.json` 的 `mcpServers`（JSONC） |
| Runtime provider | camelCase，带 `Visible` 后缀 | `cursorCliVisible` |
| 启动 / resume | argv + stdin | 空 args + prompt stdin；`agent --resume {id}` |
| 完成合同 | 无稳定 sentinel 就 Manual | `Manual` + `ResumeTerminalPolicy::Fresh` |
| 会话布局 | 有合同才扫盘 | 未固化 → tab 在、结果 `unavailable` |
| 用量抽取 | 有稳定字段才实现 | `has_usage: true`，extract 返回 `None` |
| Prompt 历史 | 有用户输入文件才加 collector | `history_source` 可登记；无 collector 则筛选为空 |
| Headless / 优化器 | catalog 位 ≠ 设置里可选 | `has_headless: true`，优化器仍只开 claude+grok |
| 无图形剪贴板贴图 | `HeadlessImagePasteKind`；必须有该 CLI 官方或源码合同，禁止抄 Claude 的 `@路径` | 见 §3.14；Codex=`bracketedPathPaste`，Pi=`typedAbsolutePath`，其余现身份=`atFileMention` |
| Plugin / marketplace | 原生 plugin 根、**本 Agent 的开关文件**、是否真会加载其他 Agent 的 registry | 只扫自己的目录；**不要**抄 Grok 去列 Claude `installed_plugins.json`，除非该 CLI 运行时确实加载。跨 Agent 翻译仍 residual |
| portable-store 挂载点 | native skills / commands；MCP 仍是该 Agent 配置 leaf（不进仓库）；是否扫其他 Agent 根 | Skill=`~/.cursor/skills`；Command=`~/.cursor/commands`；MCP=`mcp.json` `mcpServers`（native leaf + Pull）。会扫 Claude / Codex / `.agents` 则必须处理 Skill/Command 的 `loadedViaOtherPath` 与仓库行 **暗淡芯片**（§3.13）。无 L3 → apply attach/detach/migrate/destroy blocked |
| 漂移确认当前版本 | Hub 账本对齐，不写该 CLI | 必须能 preview+apply；无 L3 也不得 `MUTATION_BLOCKED`。文案「确认当前版本」 |
| 逃逸软链恢复 | 源树复制进 store，native 挂正规软链 | 必须能 preview+apply；无 L3 也不得 CLI 门禁。已装备显示问题行。文案「恢复为仓库资产」 |

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
5. **穷尽 match / 列表**（Hub 投影、portable、`plugin_enablement`、`portable_store`、`PortableAssetActionKind`/`PlanOperation`、packages、session、usage）
6. **前端类型 / decoder / i18n / 列表 helper**
7. **验证**（§6）

不要先做 UI 再补 catalog。壳层列表必须读 `allHubTargets()` / `allSessionSources()` / `allHistorySources()`。

## 3. 分面改哪些文件

下列路径以 2026-08 代码树为准。新增身份时 **每个 `match target` / `match provider` / `Record<AgentTarget, …>` 都要有新臂**；能改成读 catalog 的列表就不要再手写字面量。

### 3.1 身份目录

| 位置 | 做什么 |
|------|--------|
| `src-tauri/src/agent_catalog/mod.rs` | 加 `AgentId` variant + `IDENTITIES` 行（含 `headless_image_paste`）；更新 `catalog_registers_*` 与 `headless_image_paste_kinds_match_research` |
| `web/src/lib/agentCatalog.ts` | 同步一行（含 `headlessImagePaste`）；`headlessOptimizerProviders()` 仍白名单已实现的优化器，不要只看 `hasHeadless` |
| `web/src/lib/agentCatalog.test.ts` | hub / session / history 列表断言 + 每身份 `headlessImagePaste` |

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
| `src-tauri/src/agent_hub/support/runtime-discovery.json` | 只登记该 CLI **真实会加载**的根（对照官方 skills 文档，不要抄已有 Agent）。`pluginRegistry` / `pluginMarketplace` 今天仅 Grok 指向 Claude cache；没有运行时证据就不要加。兼容根的 `ownedBy` / `originKind` / `gatedBy` 决定仓库芯片是实心、暗淡还是描边，见 §3.13 |
| `src-tauri/src/agent_hub/support/manifest.rs` | 若有名字列表单测，补新 token |

### 3.3 Hub：AssetAdapter

复制最接近的模板到 `src-tauri/src/agent_hub/targets/<id>.rs`，然后：

1. `targets/mod.rs`：`pub mod`、`pub use`、`probe_target` match
2. 接到 `service.rs` 的 adapter vec、`projection_ops.rs`、`instructions/compiler.rs`、`user_instructions/inventory.rs`、`portable_inventory/{scanner,plugin_enablement}.rs`、`portable_store/`、`portable_actions/{planner,targets}`、`packages/{builder,adoption,activator}`、`plugins/decompose.rs`、`replication/pull.rs`、`cross_agent.rs`、`tests/agent_hub_cli_contract.rs`
3. 单测至少覆盖：公共槽不写不该写的共享文件；Claude 兼容目录不是 native；受管文件名固定；**plugin `actualEnabled` 不继承 Claude `enabledPlugins`**（§3.9）；**store 软链只跟随 `portable-store/`，逃逸拒绝**（§3.10）；**漂移确认当前版本不走 CLI**（§3.11）；**逃逸软链恢复不走 CLI、不删源树、native 变成 store 软链**（§3.12）

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
- live usage 还要求非空 `native_session_id`。Claude/Codex/OpenCode 走各自 auto-title；Grok/Gemini/Cursor/Pi 走 `workbench/auto_title_catalog.rs`，只扫该 CLI **已证实**的会话文件（Grok `active_sessions.json` + `summary.json`，Gemini `tmp/*/chats/*.json`，Cursor CLI `chats/<hash>/<chatId>/meta.json`，Pi `~/.pi/agent/sessions/**/*.jsonl`）。不要猜路径，不要把 Cursor IDE `agent-transcripts` 当 CLI 会话。占位标题也必须绑定 native id，不能等 generated_title。

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
| Agent Hub 文案 | `agentHub.json`：`targets`、`targetFilter`、三槽 placeholder、pane 名；plugin 借用 `borrowedHintPlugin` / `borrowedImpactEnablePlugin` / `borrowedImpactDisablePlugin`；store `storeAttachHint` / `storeDetachHint` / `storeMigrateHint` / `storeDestroyHint` / `storeStillLoadedVia`；漂移 `actions.confirmCurrentVersion` / `inventory.confirmAllVersions` / `actionDialog.confirmCurrentVersionHint` / `confirmAllCurrentVersionHint`；`borrowedFrom.portableStore` |
| decoder | `schemas/agentHub.ts`、`schemas/portableInventory.ts`、`schemas/orchestrator.ts` |

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
| Grok | `config.toml` → `[plugins] enabled = […]` / `disabled = […]`（Hub **可以**写这两个数组，纯文件 patch，不 spawn CLI） | `enabled` 非空则当白名单 | 只认 `disabled`；**不要**把 Claude `enabledPlugins` 或 Grok `enabled` 白名单当成借用包已关 |
| OpenCode / Gemini / Cursor / Pi | 无独立 Hub 可读开关 | 目录存在 → **开** | 永不读 Claude / Codex / Grok 的开关文件 |

写盘分流（`targets/portable.rs::mutation_target_for_action`）：

- Plugin **Enable / Disable** → **viewing** target（改当前 Agent 的标记）
- Plugin **Uninstall** → **owner**
- 未进 store 的 Skill / Command / MCP 启停卸载 → **owner**（`SharedAgents` → Codex）
- 已进 store 的 Skill / Command / MCP → **viewing** 的 native 根（建/拆链或 leaf）；`destroyStore` 才动真树

Direct-local allowlist（`portable_actions/targets/mod.rs::supports_direct_local_action`）目前只有 **Claude | Codex** 的 CLI 路径。Grok Plugin 的 `[plugins] enabled/disabled` 与 Grok/Gemini/Cursor/OpenCode **自身** MCP 的 `enabled` 是 viewing 配置 patch，Hub **可以**写这些数组/字段（不 spawn CLI、不要求 L3 `activatePackage`）。仍禁止 remap 到 `claude plugin …`，也禁止未认证的 `grok plugin` CLI（短名 `grok plugin disable superpowers` 会碰到 native 安装与 Claude cache 同名）。借用 MCP 不得 Enable/Disable/Uninstall。漂移「确认当前版本」不是这条 allowlist 上的动作，见 §3.11。

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

权威实现：`src-tauri/src/agent_hub/portable_store/`。真树只在 `<data_dir>/portable-store/`（跟 `CC_PARTNER_DATA_DIR`），**不是** `~/.agents`。Grok / Cursor / Gemini / OpenCode / Pi 运行时可能**读取** `~/.agents/skills`，但那是借用扫描根，不能当 store 真树。Claude **不**把 `~/.agents` 当技能库。`ownedBy: portableStore`。scanner 只跟随 canonicalize 落在 store 内的软链；逃逸 fail-closed。无根 `SKILL.md` 的 skill 包（如 `superpowers/`）扫描展开成带清单的子项（一层子目录 + 可选 `skills/`），`native_id` 用子项目录名；Hub 已装备/仓库把同一包合成一行，store 软链与启停仍按**包根** `skill:<包>`。

| 资产 | 真树 | 附加到该 Agent | 从此 Agent 卸下 | 本机彻底删除 |
|------|------|----------------|-----------------|--------------|
| Skill | `portable-store/skills/<id>/` | native skills 根上的**目录软链** | 只拆 viewing 这条链 | 删真树 + 清 Claude/Codex 链 |
| Command | `portable-store/commands/<id>.md` | native commands 根上的**文件软链**；无 command 根（如 Pi）→ 缺席 | 同上 | 同上 |
| MCP | **不进仓库**（遗留 `portable-store/mcp/*.json` 不自动删除、也不再投影） | 各家配置 **native leaf**（JSON `mcpServers` / TOML `mcp_servers`） | Enable/Disable/Uninstall 改当前或 owner 配置 | Uninstall 删该配置 leaf（可 snapshot 到 hub disabled）；跨 Agent 用已有 Pull |
| Plugin | 不进仓库 | 仍走 §3.9 viewing 开关；不翻译 marketplace | Enable/Disable 只改 viewing | Uninstall 仍改所有者磁盘 |

Hub 前端 Skill/Command：**范围之后、Agent 之前**有「已装备 / 仓库」一层（`assetLane`；MCP/Plugin 没有）。已装备 = 当前 Agent native + 已附加软链（仍展示运行时借用）；仓库 = `portable-store` 目录（已附加 / 未附加）。Agent 在两面都保留，因为附加状态按 viewing Agent 算。

未迁入 store 的 native Skill/Command：Claude/Codex 仍可能 **MOVE** 到 hub disabled（旧语义）。**已经是 store 软链**时，Disable/Detach/Uninstall-from-agent 必须 `unlink`，禁止 `portable_set_tree_enabled` 把真树 rename 进 `claude-assets/disabled`。

一键迁移：该 Agent 盘点里非软链 native Skill/Command → move 进 store → 原处放回软链。同名不同 hash → 保留 frontmatter `version` 较新的一份（无比对版本则比 mtime），旧树直接删除，不再阻断。**MCP 与 Plugin 不迁移**；不要把 MCP 做成 Plugin 那种 viewing 开关。

Grok（以及任何会扫 Claude / `.agents` 根的后来者）：

- 卸下只拆**自己的 native 根**（Skill/Command 软链）。
- 同一 `storeId` 若源 Agent 仍附加，本 Agent 库存去重后保留 `store.loadedViaOtherPath` + `loadedViaTarget` + warning `store_loaded_via_other_path`。文案 `storeStillLoadedVia`。
- **不得**为了本列表干净去拆 Claude / Codex / `.agents` 链或改他人 MCP leaf。

写盘门禁：无 L3 时 **只有 Claude / Codex** 能 apply attach / detach / migrateToStore / destroyStore（复用 `supports_direct_local_action`，不要为此新增 Supported write-side `TargetCapability`；且仅 Skill/Command）。新身份默认 inventory + preview，apply blocked。Windows 用 junction/symlink，禁止静默 copy 成第二份安装。漂移「确认当前版本」不走这条门禁，见 §3.11。

接入新身份时 store 最低测试：

- scanner：store 软链 Skill 的 content hash 跟随真树；指向 `/etc` 的逃逸链拒绝
- 若该 CLI 扫 Claude / Codex / `.agents`：源 Agent 已附加、本 Agent native 无链 → `loadedViaOtherPath` 或 compatibility 暗淡芯片，且源链未改
- planner/apply：无 L3 时 attach/detach/migrate/destroy 不得写该 Agent 原生目录
- MCP：scanner 不得给出 `canMigrateToStore` / `canAttach` / `canDetach` / `canDestroyStore`；Claude/Codex Enable/Disable/Uninstall 仍改该配置 leaf；跨 Agent 只走 Pull

### 3.11 Portable 一致性：确认当前版本

Hub 投影或 portable-store 物化过的资产会在磁盘上更新（CLI/plugin 升级、手改、别的工具改 MCP leaf）。`rendered_hash` 与当前 scan hash 分叉时库存标 **漂移**。刷新库存只清**假漂移**（哈希其实已经对齐、status 还钉在 Drift）。这类真更新必须由用户点 **确认当前版本**。

Hub 从未写过的独立 Skill/Command（`~/.claude/skills`、`~/.agents/skills` 等上游 CLI 自更新目录）刷新时 **跟随磁盘**：`ensure_managed` 把 `rendered_hash` / `observed_external_hash` 对齐到当前观测值并标 `Synced`，不标漂移、不弹确认。store 附加、物化包路径、Plugin、MCP 不走这条。

权威实现：`portable_actions` 的 `ConfirmCurrentVersion` / wire `confirmCurrentVersion`；`is_hub_ledger_only()`。apply 只 `upsert_materialization`：把 viewing target 的 `rendered_hash` 与 `observed_external_hash` 写成当前 `content_hash`（否则 `tree_hash`），`status=Synced`。**不写 Agent 磁盘、不 spawn CLI、不跟随任意 symlink。** 对账按 **当前查看 Agent** 的 materialization 行，不改 owner、不改另一家账本。

| 可做 | 不可做 |
|------|--------|
| `managementState=drifted` 且有 canonical + observed hash | 一致 / 冲突 / 不支持 / 待纳入 |
| Skill / Command / Plugin / MCP（含 store 软链与 native） | 假装恢复 Hub CAS 覆盖磁盘（本动作不写文件） |
| 无 L3 的 Grok / OpenCode / Gemini / Cursor / Pi 也能 apply | 绑到 `supports_direct_local_action` 或 CLI `MUTATION_BLOCKED` / `CLI_NOT_INSTALLED` |
| 按钮「确认当前版本」/ `Confirm current version` | 「接受磁盘」「重记哈希」 |

和邻近路径的边界：

- **不是** 指令三栏 `externalDrift`（那是用户级指令投影）。
- **不是** store attach/detach/migrate/destroy（那些才写 native 根 / leaf，无 L3 仍 blocked）。
- Enable/Disable 同字节 MOVE 不是漂移。假漂移：刷新即可，不要逼用户点确认。
- Hub 从未写过的独立 Skill/Command：刷新跟随磁盘，不要当成漂移去确认。
- `canConfirmCurrentVersion` 在 reconcile 之后才置位，scanner 默认 false。Plugin store 动作仍不支持；确认当前版本**可以**用于 Plugin（包字节可变）。

穷尽 match：Rust `PortableAssetActionKind` / `PortableAssetPlanOperation`；前端 type + decoder + `agentHub.json` 中英键；行内动作优先这条。planner 必须对 drift **豁免**本动作（其它 mutation 仍 `SOURCE_DRIFTED`）。库存页「刷新库存」下方提供 **全部迁入仓库**（仅 Skill/Command）与 **全部确认版本**：前者一次 preview/apply 当前 Agent、当前类别快照里所有 `canMigrateToStore` 项（移入便携仓库并留下软链）；后者一次确认所有 `canConfirmCurrentVersion` 项。两者都不受搜索/一致性筛选裁切；Plugin component 除外。

接入新身份时最低测试：

- planner：该 target `mutationCapability=blocked` 且未安装 CLI 时，漂移项 preview `confirmCurrentVersion` 的 `blockingReasons` 为空，`change.target` 为 viewing
- 一致项 preview 本动作必须 `NOT_DRIFTED` 或 `CANNOT_CONFIRM_CURRENT_VERSION`
- apply 不 spawn ProcessRunner；materialization 哈希对齐后 rescan 为 `hubManaged`

### 3.12 Portable 布局修复：恢复逃逸软链为仓库资产

Scanner 对 canonicalize 落在 `portable-store/` 外的 Skill/Command 根软链 fail-closed：`store_symlink_escape` + `source_blocked`，不跟随哈希。这些项在已装备列表以问题行出现（即使 `actualEnabled=false`），不能 Enable，直到 native 路径变成指向仓库的正规软链。

权威实现：`portable_actions` 的 `MaterializeEscapeLink` / wire `materializeEscapeLink`；`is_escape_link_repair()`。apply 在共享 executor 里走 `execute_skill_or_command_store`：复制逃逸目标进 `<data_dir>/portable-store/`，再把 native 路径换成 store 软链。**不删原目标、不 spawn CLI、不 remap 到另一家 executor。** 断链时才回退 `data_dir/{claude,codex}-assets/disabled/{skills,commands}/<id>` 与 `~/.agents/skills|commands/<id>`。StoreLink 拒绝。Regular 非软链幂等跳过。

| 可做 | 不可做 |
|------|--------|
| Skill / Command 根上的 `EscapeLink`（含 `source_blocked` / `unsupported`） | Plugin / MCP；plugin 内部 `AGENTS.md → CLAUDE.md` |
| 无 L3 的 Grok / OpenCode / Gemini / Cursor / Pi 也能 apply | 绑到 `supports_direct_local_action` 或 CLI `MUTATION_BLOCKED` |
| 按钮「恢复为仓库资产」/ `Restore into store` | 「接受磁盘」、把 native 真树 rename 进仓库、删除 `~/.agents` |

和邻近路径的边界：

- **不是** `migrateToStore`（那条把 native **真树 rename** 进 store；本动作 **复制** 逃逸源，源树留下）。
- **不是** §3.11 确认当前版本（那只改账本）。
- `canMaterializeEscapeLink` 在 scanner 发现 `store_symlink_escape` 后置位，并关掉 enable/disable/uninstall/store 能力。行内动作优先这条，即使 management 是 unsupported。

穷尽 match 同 §3.11。已装备页 Skill/Command 提供 **全部恢复为仓库资产**：一次 preview/apply 当前 Agent、当前类别快照里所有 `canMaterializeEscapeLink` 项，不受搜索/一致性筛选裁切。

接入新身份时最低测试：

- planner：该 target `mutationCapability=blocked`、`management=unsupported` 时 preview `materializeEscapeLink` 的 `blockingReasons` 为空
- apply 不 spawn ProcessRunner；native 路径是指向 `portable-store/` 的软链；原目标仍在；rescan `storeAttached=true` 且无 `store_symlink_escape`

### 3.13 仓库芯片借用：只画该 CLI 真会加载的根

仓库行上每个 Hub Agent 一颗芯片（`portableStoreAgentChipState`）：

| 外观 | 判定 | 含义 |
|------|------|------|
| 实心 | `storeAttached` 或本 Agent native 启用 | 挂在**自己的** native / Codex `legacyStandalone` 根 |
| 暗淡（`derived`） | `isPortableBorrowedRuntimeItem` 且未 `storeAttached` | 运行时看得到，但是别人的目录；芯片只读 |
| 描边 | 扫描表没有这项，或未启用 | 该 CLI **不会**加载这份资产 |

借用判定（Rust `is_borrowed_runtime_origin` / 前端 `isPortableBorrowedRuntimeItem`）：`originKind=compatibility`、`ownedBy` 是别的 Hub Agent、或 `ownedBy=sharedAgents`。Codex 自己的 `~/.agents/skills` 必须是 `legacyStandalone` + `ownedBy=codex`，**不是**借用。漂移 / `nativeOutputCandidate=false` 单独不构成借用。

**禁止**为对称把 Cursor 的兼容集（Claude + Codex + `.agents`）抄给后来者。芯片跟 `runtime-discovery.json` 走，表跟官方 skills 文档走。

当前已登记身份（Skill，对照官方文档，2026-08）：

| Agent | native | 会借用（compatibility） | 明确不扫 |
|-------|--------|-------------------------|----------|
| Claude | `~/.claude/skills`、项目 `.claude/skills` | 无 | `.agents` / `.codex` |
| Codex | `~/.codex/skills`；`~/.agents/skills` 为 **本 Agent** `legacyStandalone` | 无 | `.claude` |
| Cursor | `.cursor/skills` | Claude + **Codex** + `.agents`（user/project） | — |
| Grok | `.grok/skills` | Claude；用户级 `~/.agents/skills` 与 `~/.agents/commands` | **`.codex`**（官方未列） |
| Gemini | `.gemini/skills` | 仅 `.agents` 别名 | Claude / Codex |
| OpenCode | `.opencode` / config-root `skills` | Claude + `.agents`。门控：`OPENCODE_DISABLE_EXTERNAL_SKILLS` 关全部外部；`OPENCODE_DISABLE_CLAUDE_CODE` / `_SKILLS` **只关** `.claude`，不得拿来关 `.agents` | `.codex` |
| Pi | `{piConfigRoot,project/.pi}/skills` | `.agents` 无条件；`~/.claude/skills` 与 `{codexConfigRoot}/skills` 仅当 `settings.json` 的 `skills` 数组 **点名该条路径**（点名 Claude 不得打开 Codex） | 未点名的 Claude/Codex 根 |

接入核对清单：

1. 打开该 CLI 官方 skills 文档，列出它会加载的每一个目录。
2. 只把这些目录写进 `runtime-discovery.json`。没有文档证据就不要加 `~/.codex/skills` 或 Claude plugin registry。
3. 兼容根：`originKind=compatibility`，`ownedBy` 为真实所有者（Claude / Codex / `sharedAgents`），`nativeOutputCandidate` 由 stamp 自动关掉。
4. 官方有环境变量 / settings 门闩时用 `gatedBy`，语义必须与官方一致；不要把「关 Claude」绑到 `.agents`。
5. Codex 的 `~/.agents` 对 Codex 自己不是借用；对 Cursor/Grok/Gemini/OpenCode/Pi 才是 `sharedAgents` 借用。
6. 单测至少：native 不是 compatibility；会加载的兼容根能发现且 `ownedBy` 正确；不会加载的根保持缺席；门控开/关各一条。
7. 仓库芯片：源 Agent 附加后，只有扫描表包含该挂载路径的 Agent 才应变暗淡。

### 3.14 Workbench 无图形剪贴板贴图

有图形剪贴板时所有 Agent 共用合同：owning device 写 OS 剪贴板，再向 PTY 注入 Ctrl+V（`\x16`）。Linux 无可达 X11/Wayland（SSH / headless / 过期 `DISPLAY=localhost:10.0`）时 **禁止** 再发 Ctrl+V，也 **禁止** 一律打 Claude 的 `@路径`。

必须在身份表填 `headless_image_paste` / `headlessImagePaste`，取值只允许：

| kind | PTY 注入 | 何时用 |
|------|----------|--------|
| `atFileMention` | `@/abs/path ` | TUI 把 `@` 当文件/图片引用（Claude、OpenCode、Grok、Gemini、Cursor） |
| `bracketedPathPaste` | CSI `200~` + 绝对路径 + `201~` | TUI 把 **pasted 图片文件路径** 转成附件（Codex `normalize_pasted_path`） |
| `typedAbsolutePath` | ` /abs/path ` | TUI 原生是把路径插入编辑器，由 read 工具读图（Pi） |

探测顺序（`detect_session_agent`）：

1. 该 terminal 的 active Agent runtime `provider_id` → `identity_by_runtime`（genericTerminal 得到 None）
2. tmux 活动 pane `#{pane_current_command}` → `identity_by_executable_name`（basename，去 `.exe`，更长名字优先，故 `cursor-agent` 赢过 `agent`）
3. 都认不出 → `AtFileMention`（当前多数身份；**新身份不得依赖这个默认**，必须自己填表）

接入核对：

1. 读该 CLI 官方键盘/贴图文档或 TUI 源码：Ctrl+V 读剪贴板？`@path`？路径 paste？插入裸路径？
2. 无显示时选上表一种；没有合同就先不要宣称支持，不要抄 Claude。
3. Rust / TS 身份表同一行必须填 `headless_image_paste`；`headless_image_paste_kinds_match_research` 与前端 `registers headless image paste kinds` 必须加断言。
4. `identity_by_executable_name` 覆盖该 CLI 的 `executable_names`（含 Windows `.exe`）。
5. 禁止在 `screenshot/clipboard.rs` 再写死 `claude` 字符串或第二种 `@` 语法。

现身份调研摘要（2026-08-26）：

| Agent | 有显示 | 无显示注入 | 依据 |
|-------|--------|------------|------|
| Claude | Ctrl+V | `@/abs/path ` | 官方 `@` 文件引用；prompt 里写路径也可读图 |
| Codex | Ctrl+V / Win Alt+V | bracketed 绝对路径 | TUI `normalize_pasted_path` 把 pasted image path 变成附件；`@` 不是这条路径 |
| OpenCode | Ctrl+V（wl-paste/xclip） | `@/abs/path ` | TUI `@file` 引用；社区贴图也走 `@` |
| Grok | Ctrl+V | `@/abs/path ` | 官方 `@` 附加文件；复制文件再粘贴走路径 |
| Gemini | Ctrl+V / Win Alt+V | `@/abs/path ` | 官方 `@` 可引用图片；pasted path 会加 `@` |
| Cursor CLI | Ctrl+V（macOS） | `@/abs/path ` | `@` 与 prompt 内路径；headless 文档写「prompt 里放路径」 |
| Pi | Ctrl+V（写临时文件再插入路径） | ` /abs/path ` | 官方 compose：粘贴/拖放变成路径，模型 `read`；`@` 是自动完成不是贴图合同 |

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

# 借用根是否抄了别家套餐 / Codex ~/.agents 是否误标 sharedAgents
rg -n '"originKind": "compatibility"' src-tauri/src/agent_hub/support/runtime-discovery.json
rg -n 'ownedBy": "sharedAgents"' src-tauri/src/agent_hub/support/runtime-discovery.json
rg -n "OPENCODE_DISABLE_EXTERNAL_SKILLS|piSettingsSkills" src-tauri/src/agent_hub/support/runtime-discovery.json
rg -n "isPortableBorrowedRuntimeItem|portableStoreAgentChipState" web/src/pages/AgentHub

# portable-store 是否漏新 Agent 挂载 / 是否把 ~/.agents 当 store
rg -n "portable_store_root|PortableAssetOwner::PortableStore" src-tauri/src/agent_hub
rg -n "ownedBy: 'portableStore'|loadedViaOtherPath" web/src

# 确认当前版本是否漏穷尽臂 / 是否被误绑成 native 写入
rg -n "ConfirmCurrentVersion|confirmCurrentVersion|is_hub_ledger_only" src-tauri/src/agent_hub
rg -n "confirmCurrentVersion" web/src

# 逃逸软链恢复是否漏穷尽臂 / 是否被误绑成 CLI 或删除源树
rg -n "MaterializeEscapeLink|materializeEscapeLink|is_escape_link_repair|restore_escape_into_store" src-tauri/src/agent_hub
rg -n "materializeEscapeLink|isPortableEscapeLinkItem" web/src

# user-mirror 是否漏新身份 / 是否静默跳过
rg -n "all_hub_targets" src-tauri/src/agent_hub/user_mirror src-tauri/src/agent_catalog

# 无图形贴图是否漏身份 / 是否写死 Claude @路径
rg -n "headless_image_paste|headlessImagePaste" src-tauri/src/agent_catalog web/src/lib/agentCatalog.ts
rg -n "AtFileMention|BracketedPathPaste|TypedAbsolutePath" src-tauri/src/agent_catalog src-tauri/src/screenshot
```

文档：根 `AGENTS.md` 产品一句、`src-tauri/AGENTS.md` 的 `targets/` 文件名单。不要在文档里宣称 L3 真机写盘已认证。不要写「关掉 Claude plugin 等于关掉所有 Agent」。不要写「`~/.agents` 就是全 Agent 统一库」。不要把「确认当前版本」写成会改写磁盘或需要 L3。不要把「恢复为仓库资产」写成会删除源树或把 native 真树 rename 进仓库。用户发起的 user-mirror 可以写该身份用户级白名单文件 / MCP leaf / viewing Disable，但不得 spawn 未认证 CLI；新身份缺席 `all_hub_targets` 必须 fail-closed，不得跳过。

## 5. 能力状态怎么填

| 状态 | 用在哪 | 用户应看到 |
|------|--------|------------|
| 支持 | 有合同且已实现 | 正常列表 / 可启动 |
| scan-only / blocked | Hub 写无 L3 | 能扫、不能保存到原生文件；稳定错误码 |
| unavailable | CLI 未装或目录未知 | tab 在，诊断文案，不是空白崩溃 |
| residual | 跨 Agent 不可翻译的 Plugin/Hook | 计划项标 residual，不标成功 |
| 缺席 | 该面根本不投影 | catalog 对应字段 `None` / `false` |

打开原生写入的条件：quality-matrix 里有对应 `L3-…` evidence，并且 `support-manifest` 的 `capabilityEvidenceIds` 指向它。未认证保持 blocked。§3.11 确认当前版本**不是**原生写入，不要用这张表把它挡住。

## 6. 验证

```bash
# Rust：身份 + 新 adapter + 路径覆盖 env
cd src-tauri
cargo fmt --all
cargo check --locked --all-targets
cargo test --locked --lib -- agent_catalog::
cargo test --locked --lib -- runtime_discovery
cargo test --locked --lib -- targets::<id>::
cargo test --locked --lib -- plugin_enablement
cargo test --locked --lib -- agents_without_plugin_flags
cargo test --locked --lib -- portable_store
cargo test --locked --lib -- grok_unattached_store
cargo test --locked --lib -- confirm_current_version

# 前端
cd web
npm run check:i18n
npx --no-install tsc -b --pretty false
npx --no-install vitest run src/lib/agentCatalog.test.ts \
  src/pages/AgentHub/crossAgent/crossAgentPresentation.test.ts \
  src/pages/AgentHub/portableAssets/portablePullPresentation.test.ts \
  src/pages/AgentHub/portableAssets/portableInventoryPresentation.test.ts
```

本机若已有 `tauri dev` 占用 `src-tauri/target`，**等待** `Blocking waiting for file lock on artifact directory`，不要换空的 `CARGO_TARGET_DIR=/tmp/...`（会从 `unicode-ident` 起全量重编，比等锁更慢）。rust-analyzer 已使用独立的 `target/rust-analyzer`。验证用：

```bash
./scripts/cc-partner-cargo.sh check --locked --lib --tests
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
| Portable | `.cursor/skills`、`.cursor/commands`、`mcp.json` → `mcpServers`（JSONC）；兼容扫描 Claude + **Codex** + `.agents`（官方 skills 目录）；无独立 plugin 开关，不得继承 Claude `enabledPlugins`；portable-store 可盘点，apply attach 仍 blocked（无 L3）；漂移「确认当前版本」仍可 apply（只改 Hub 账本） |
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
| Portable | `.pi/skills`、`~/.pi/agent/skills`；无条件 `~/.agents/skills`；Claude / Codex skills 仅 settings `skills` 点名该路径；无内建 MCP，不伪造 `mcp.json`；无 command 根（store Command 缺席）；无独立 plugin 开关，不得继承 Claude `enabledPlugins`；store apply blocked；漂移「确认当前版本」仍可 apply |
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
- 不要把 Cursor 的 Claude+Codex+`.agents` 兼容集抄给不会加载 `.codex` 的 Agent（Grok / Gemini / OpenCode / Pi 默认都不扫 Codex 目录）
- 不要用 `OPENCODE_DISABLE_CLAUDE_CODE_SKILLS` 去关 `.agents`；OpenCode 关外部技能用 `OPENCODE_DISABLE_EXTERNAL_SKILLS`
- 不要把 Pi settings 点名 `.claude/skills` 当成也打开了 `~/.codex/skills`
- 不要把 Codex 自己的 `~/.agents/skills` 标成 `sharedAgents` 借用（那是 Codex `legacyStandalone`）
- 不要把 Plugin marketplace 翻译成另一家的 plugin，也不要把 Plugin 或 MCP 迁入 portable-store
- 不要把 MCP 做成 Plugin 那种 viewing 开关；MCP 启停/卸载走当前或 owner 配置 leaf，跨 Agent 用已有 Pull
- 不要在 `runtime-discovery.json` 里给不会加载 Claude plugin 的 Agent 加 `pluginRegistry`
- 不要把 Claude `enabledPlugins`（或 Codex/Grok 白名单）当成其他 Agent 的 `actualEnabled`
- 不要在无 L3 / 无本 target executor 时，把 Enable/Disable 映射到所有者 CLI（例如 Grok 列表调用 `claude plugin disable`）
- 不要把 native plugin 白名单套到借用包（缺席 ≠ 已关）
- 不要把 `~/.agents` 当成 Claude 的技能库，也不要把它当成 portable-store 真树；store 只在 `<data_dir>/portable-store/`。Grok / Cursor / Gemini / OpenCode / Pi 运行时读取 `~/.agents` 时，Hub 必须按借用扫描，不得漏标、也不得写成 native
- 不要跟随任意 symlink，只跟随目标在 `portable-store/` 内的链
- 不要为「从会扫 Claude 的 Agent 卸下」去改 Claude 磁盘
- 不要把 store 软链 Disable 做成 MOVE 真树进 disabled
- 不要把「确认当前版本」当成 native 写入：无 L3 也必须能 apply；不要绑 `supports_direct_local_action` 或 remap 到另一家 CLI
- 不要把「恢复为仓库资产」当成 CLI 动作或删除 `~/.agents`；无 L3 也必须能 apply；不要把它做成 `migrateToStore` 那种 native 真树 rename
- 不要用刷新库存冒充接受外部更新（刷新只清假漂移）
- 不要把 portable 漂移和指令三栏 `externalDrift` 做成同一条路
- 不要把按钮写成「接受磁盘」或「重记哈希」；用「确认当前版本」；逃逸软链用「恢复为仓库资产」
- 不要静默 copy 成第二份安装来绕过 Windows symlink 权限
- 不要在同一槽写两个落点「以防万一」
- 不要把 `has_headless` 直接接到 Prompt 优化下拉（Gemini/Cursor 都登记了，但优化器未开放）
