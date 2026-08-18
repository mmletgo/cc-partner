# Agent 身份目录与全表面能力适配

- 日期：2026-08-16
- 状态：已确认（待用户审阅成文 spec）
- 依赖：
  - `2026-07-15-agent-adapter-platform-design.md`
  - `2026-07-15-agent-metadata-ledger-design.md`
  - `2026-07-29-multi-cli-agent-hub-design.md`
  - `2026-08-10-agent-hub-correction-design.md`
- 对应计划：实现前另写 `docs/superpowers/plans/2026-08-16-agent-capability-catalog.md`
- 共享边界：固定 LAN、无调用者身份鉴权；capability 只表示协议/CLI 能力，不表示设备可信。

## 1. 问题

cc-partner 已按 Claude / Codex / OpenCode 适配了运行时、会话搜索、自动标题、用量账本、Prompt 历史、Agent Hub、跨 Agent 适配、编排器 catalog 与 Prompt 优化。这些面各自维护身份枚举：

- Hub：`AgentTarget` = `claude | codex | opencode`
- Runtime：`AgentProviderId` = `claudeCodeVisible | codexVisible | genericTerminal | openCodeVisible`
- 会话搜索：`AgentSessionSource` = `claude | codex | opencode`
- Prompt 历史：`source` = `claude | codex | opencode`
- 前端多处写死上述三元组

再接入 Grok Build（可执行 `grok`）与 Gemini CLI（可执行 `gemini`）时，若继续复制枚举，会把第四、第五套身份再散到每个功能里。同时，这两个 CLI 的指令文件、Skill/MCP 布局、完成信号与 Claude 不对等，不能假装「实现一个超级 Adapter 就获得全部功能」。

## 2. 目标

1. 引入编译期 **Agent 身份目录**：一个 `AgentId` 投影到 Hub / Runtime / Session / History / Usage / Headless 等可选身份。
2. 所有「按 agent 分叉」的产品面改为问能力、不问名字；UI 列表来自目录，禁止再写死三家白名单。
3. 为缺 trait 的面补小合同：`SessionHistory`、`UsageSource`、`HistoryCollector`、`HeadlessCompletion`；已有 `AgentAdapter` 与 `AssetAdapter` 继续分治，不合并成 God Trait。
4. 在同一 V1 内把 Grok Build 与 Gemini CLI 接到**全部**现有适配面：能做的做实，不能做的以 scan-only / residual / unavailable / 缺席出现在同一套界面，禁止从 Hub / 历史 / 统计里把新 agent 藏掉。
5. 未知 wire token fail-closed，禁止静默回退 Claude。
6. 现有 Claude / Codex / OpenCode 行为 characterization 不变。

## 3. 非目标

- 不引入可切换 LAN 模式、鉴权矩阵或把 peer 称为已认证设备。
- 不自动安装 CLI、不读取或同步 API key / `~/.grok/auth.json` / Google 登录态。
- 不把 Grok Plugin marketplace 或 Gemini extensions 翻译成 Claude/Codex/OpenCode Plugin。
- 不把一家 CLI 的 plugin 开关当成另一家的启用态（Claude `enabledPlugins=false` 不等于 Grok/Codex 已关）。借用 Claude plugin 包时，盘点可以显示，Enable/Disable 只写 **当前查看 Agent** 的标记。
- 不把 Grok 的 Claude 兼容扫描目录（`~/.claude/skills` 等）再写一份 Grok 副本。
- 不为 Grok / Gemini 做 OpenCode 式项目内 runtime bridge。
- 不把 Antigravity CLI 登记为 `gemini` 的别名；若日后二进制改名，另开 `AgentId`。
- 不把 `cc-switch` / `internal_claude`（API 供应商切换）并进身份目录。
- 不把用户自建 Prompt 库、传输、截图、速记本、Git、浏览器预览改成按 agent 分叉。
- 不在本 spec 宣称 L3 真机写盘 / 真机 TUI 已认证。
- 不把 `~/.agents` 当成 Claude / Grok 的 portable-store 真树；共享仓库只在 `<data_dir>/portable-store/`。
- 不为 Grok / OpenCode / Gemini / Cursor / Pi 开放 portable-store 原生写入（无 L3 则 inventory/preview only）。「确认当前版本」不是原生写入：无 L3 仍可 apply，只对齐 Hub 账本。
- 不把 Plugin marketplace / 启用标记翻译进 portable-store；Plugin 仍是 viewing-agent residual。
- 不把 MCP 迁入 portable-store，也不把 MCP 做成 Plugin 那种 viewing 开关；MCP 是各家配置 native leaf，跨 Agent 走已有 Pull。
- 不把 portable 漂移「确认当前版本」绑到 `supports_direct_local_action` 或 remap 到另一家 CLI；也不把它与指令三栏 `externalDrift` 做成同一条路。

## 4. 身份目录

前后端各有一份同源 catalog（Rust 权威，前端类型/decoder 对齐）。目录是编译期表，不是用户配置，也不是 P2P 上传的插件。

### 4.1 `AgentId`

稳定小写 token：`claude` | `codex` | `opencode` | `grok` | `gemini`。

显示名：Claude Code / Codex / OpenCode / Grok Build / Gemini CLI。

### 4.2 投影字段

每个 `AgentId` 可缺省下列投影；缺省表示该面不出现。

| 字段 | 用途 | wire 例子 |
|------|------|-----------|
| `hubTarget` | Agent Hub、跨 Agent、support-manifest | `grok` / `gemini` |
| `runtimeProvider` | 可见终端、编排器、hint | `grokBuildVisible` / `geminiCliVisible` |
| `sessionSource` | ⌘K 搜索、自动标题 | `grok` / `gemini` |
| `historySource` | Prompt 历史采集与筛选 | `grok` / `gemini` |
| `usageExtractor` | Token 统计、会话用量回填 | 与 `runtimeProvider` 对齐 |
| `headlessProvider` | Prompt 优化 | `grok` / `gemini`（可选） |
| `executableNames` | probe 候选 | `["grok"]` / `["gemini"]` |

`genericTerminal` 只存在于 Runtime，没有 `AgentId` 行，也不进搜索 / 历史 / Hub。

### 4.3 登记表

| AgentId | Hub | Runtime | Session | History | Usage | Headless |
|---------|-----|---------|---------|---------|-------|----------|
| `claude` | `claude` | `claudeCodeVisible` | `claude` | `claude` | 有 | 有（默认） |
| `codex` | `codex` | `codexVisible` | `codex` | `codex` | 有 | 无 |
| `opencode` | `opencode` | `openCodeVisible` | `opencode` | `opencode` | 有 | 无 |
| `grok` | `grok` | `grokBuildVisible` | `grok` | `grok` | 有 | 有 |
| `gemini` | `gemini` | `geminiCliVisible` | `gemini` | `gemini` | 有则接 | 结构化输出稳才开放 |

### 4.4 必须删除的硬编码

实现时把下列集合改为读 catalog（列举为验收清单，不是完整文件表）：

- Rust：`AgentTarget`、`AgentProviderId`、`AgentSessionSource`、`SOURCE_*`、`scan_once` 源列表、support-manifest `targets[]`、编排器 registry `with_defaults`
- 前端：`web/src/lib/types/agentHub.ts`、`web/src/lib/types/core.ts` 的 `CcHistorySource`、`WorkbenchSessionSearch` 的 `SESSION_SEARCH_SOURCES`、`web/src/api/workbench.ts` source 联合类型、`PortableInventoryRow` 的三列 Record、Agent Hub `AGENT_TARGETS`、orchestrator decoder、Token 统计 provider 标签

未知 token：parse/decoder 失败，不映射 Claude。

旧库行 / 旧同步体：`source` 缺省仍为 `claude`；已认识的新 token 原样保存。旧 GUI 收到未知 `AgentTarget` 时 fail-closed（现有 decoder 已如此），不要求 N-1 能管理 Grok/Gemini Hub。

## 5. 能力合同

每个合同只服务一个产品面。新 agent 实现该面，或登记为 unsupported / scan-only / residual / unavailable / 缺席。UI 必须能展示后四态。

### 5.1 Runtime — 已有 `AgentAdapter`

保持 `probe` / `build_launch_plan` / `build_resume_plan` / `normalize_runtime_event` / `extract_usage` / `interrupt_input` / `supports_resume` / `supports_usage` / `completion_contract` / `resume_terminal_policy`。

Grok / Gemini：

- probe：`grok --version` / `gemini --version`，超时与截断对齐现有 adapter（2s / 4KiB）
- 交互启动：可执行名、空 args；**允许空 prompt**（用户自己在 TUI 里打字）
- 编排器启动：stdin 注入 prompt；`completion_contract = Manual`；禁止 Sentinel/Hook 猜测完成
- resume：`grok --resume <uuid>` / `gemini --resume <uuid>`；`ResumeTerminalPolicy::Fresh`（全屏 TUI 占 PTY）
- 中断：`Ctrl-C`（`\u{3}`）
- 不做项目内 hook 桥；`supports_usage` 仅当 UsageSource 能从磁盘或事件抽出非空 snapshot 时为 true

本机未安装时 catalog 行仍在，availability = unavailable，reason = `provider_unavailable`。

### 5.2 SessionHistory + 自动标题

新 trait（名称实现时可定为 `SessionHistoryAdapter`），迁入现有 Claude jsonl / Codex rollout / OpenCode sqlite。

最小方法：

- `source_id() -> sessionSource`
- `search(worktree_path, query, limit) -> SessionSearchResult`（复用现有 Hit/Diagnostics DTO）
- `preview(native_session_id) -> SessionPreview`
- `resume_plan(native_session_id) -> { executable, args }`
- `title_hint(native_session_id) -> Option<String>`（自动标题 poller 调用）

**Grok 磁盘**（已对照本机 `~/.grok/sessions`）：

```
~/.grok/sessions/<url-encoded-cwd>/<session-id>/
  summary.json      # info.id、info.cwd、generated_title、session_summary、current_model_id、时间
  updates.jsonl     # 对话权威日志
  signals.json      # token / turn
```

按 worktree path 与 `summary.info.cwd` / 目录 encoded-cwd 匹配。搜索 title + user/assistant 文本。resume 用 `info.id`。标题优先 `generated_title`，否则 `session_summary`。

**Gemini 磁盘**（官方合同）：

```
~/.gemini/tmp/<project_hash>/chats/
```

`gemini --resume <uuid>`。实现时对照 `google-gemini/gemini-cli` 当前源码钉死 `project_hash`；对不上则枚举 `tmp/*/chats`，用记录内 cwd 过滤，禁止猜 hash。标题用官方 list 标题或首条 user prompt。

未安装或目录不存在：`diagnostics.status = unavailable`，与现有远端离线语义一致。搜索 tab 仍列出该 source。

### 5.3 UsageSource + Token 统计

新 trait，迁入 `agent_usage.rs` 三套抽取函数。

- `extract(native_session_id, cwd) -> Option<ReliableUsageSnapshot>`
- 抽不到返回 `None`；ledger / Token 统计显示「未提供」；**禁止把缺失写成 0**

Grok：读对应 session 的 `signals.json`。Gemini：仅当 chat/session JSON 有稳定 input/output/cached 字段时抽取。Token 统计页的 provider 筛选与三维拆分标签来自身份目录，不写死四家。

Ledger 存储保持 provider-neutral；新 runtime token 写入 `provider_id`。兼容别名只保留现有 Claude/Codex/OpenCode 的历史混用，不为 Grok/Gemini 再发明第二套短名。

### 5.4 Prompt 历史 — `HistoryCollector`

现有 `cc/collector.rs::scan_once` 已顺序跑 Claude + Codex + OpenCode。V1 增加 `cc/sources/grok.rs` 与 `cc/sources/gemini.rs`。

合同：

- 只采用户直接输入；过滤系统/skill/sidechain/工具结果（对齐 Codex `is_systemish_user_text` 与 Claude `is_user_authored`）
- 主键带源前缀：`grok:{session_id}:{msg_id}` / `gemini:{session_id}:{msg_id}`，避免与 Claude `{session}:{uuid}` 碰撞
- `ClaudeHistoryRow.source` 增加稳定常量 `SOURCE_GROK` / `SOURCE_GEMINI`
- 项目身份继续走 Git 主工作区，不猜目录名
- 某源失败只记日志，不阻断其它源
- 前端 `CcHistorySource` 与筛选下拉读 catalog
- 局域网 / GitHub 同步不新增协议：`source` 已是字符串；旧对端不认识新值时保持现有 fail-closed / 透传，不另做兼容层

表名可继续叫 `claude_history`（历史包袱），产品文案与 PRD 改为「Prompt 历史」，筛选展示各 agent 显示名。

### 5.5 Agent Hub — 扩展 `AgentTarget` + 两个 `AssetAdapter`

V1 **必须**把 `AgentTarget` 扩为五值，并把 Grok / Gemini 放进壳层 agent 切换器、support-manifest、跨 Agent 目的地。写能力仍遵守「无 quality-matrix evidence = scan-only / blocked」。

#### 5.5.1 Grok 指令投影

Grok 同时加载 `AGENTS.md`、`CLAUDE.md` / `Claude.md` / `CLAUDE.local.md`，以及：

- 用户级：`~/.grok/rules/*.md`（`$GROK_HOME` 可覆盖）
- 项目级：`<dir>/.grok/rules/*.md`

因此 **公共槽不得再写一份会与 Codex/OpenCode 抢 `AGENTS.md` 的文件**。

| 槽 | Grok 落点 | 原因 |
|----|-----------|------|
| common | 不单独物化 | Grok 会读已有 `AGENTS.md` / `CLAUDE.md`；Hub 对 Codex/OpenCode 的 common 投影即对 Grok 生效 |
| adapted | 用户级 `~/.grok/rules/cc-partner.adapted.md`；项目级 `<root>/.grok/rules/cc-partner.adapted.md` | 专属语义，不进共享 AGENTS.md |
| exclusive | `…/cc-partner.exclusive.md`（同上两级） | 同上 |

扫描仍读取原生 AGENTS.md / CLAUDE.md / 已有 `.grok/rules/*`，用于纳管和对账。受管文件用固定文件名，便于 ownership 识别；不得改写用户其它 rules 文件。

Grok 的 Claude 兼容会再扫 `~/.claude/` 与 `.claude/`。Hub **禁止**为 Grok 再往这些 Claude 路径写盘。

#### 5.5.2 Gemini 指令投影

Gemini 不读 `AGENTS.md`。指令入口是 `GEMINI.md`（项目）与 `~/.gemini/GEMINI.md`（用户级）。`settings.json` 的 `context.fileName` 默认含 `GEMINI.md`。

| 槽 | Gemini 落点 |
|----|-------------|
| common | 项目 `GEMINI.md` 的 common 编译结果（用户级则 `~/.gemini/GEMINI.md` 的 common 段） |
| adapted | 同文件内适配块；若实现时确认 Gemini 会加载 `.gemini/*.md`，可改为 `.gemini/cc-partner.adapted.md`，选定后写死一种，禁止双写 |
| exclusive | 同文件 exclusive 段，或 `.gemini/cc-partner.exclusive.md`（规则同上，只选一种） |

实现时先对照当前 Gemini CLI 的 context 加载列表再锁定 adapted/exclusive 是「同文件分块」还是「侧车 md」。锁定后写入 support-manifest 与 adapter 单测，本 spec 允许二选一，但 **禁止同一槽写两个落点**。

#### 5.5.3 可移植资产

| 资产 | Grok | Gemini |
|------|------|--------|
| Skill | `~/.grok/skills`、`<repo>/.grok/skills` 的 `SKILL.md`；扫描必做；evidence 齐才投影 | `~/.gemini/skills`、`<repo>/.gemini/skills`；同上 |
| Command | `~/.grok/commands` 的 `*.md` | `~/.gemini/commands` |
| MCP | `~/.grok/config.toml` 的 `[mcp_servers.*]`，复用已有 TOML patch | `~/.gemini/settings.json` 或项目 `.gemini/settings.json` 的 `mcpServers`，复用 JSONC patch |
| Plugin | marketplace 模型；V1 只读盘点 + 跨 Agent residual。Grok 可按 `runtime-discovery` 列出 Claude `pluginRegistry`，但 `actualEnabled` 只读 Grok `[plugins]`，禁止继承 Claude `enabledPlugins`。无 L3 不得打开 Hub 内 Grok plugin 启停 | extensions；同样 scan-only / residual；不得继承 Claude 开关 |

跨 Agent：目的地列表 = catalog 中所有 `hubTarget != None`。指令公共正文与 Skill 正文可以流向 Grok/Gemini；Plugin / Hook / OpenCode JS/TS/npm **必须 residual**，只回写 source target。Grok 作为目的地时，common 指令若已存在于 `AGENTS.md`，计划项标 skip（已由其它 target 物化），不重复写入。

`shared` 资产对五端可见；`targetOnly` 仍严格隔离。

本机一份的用户 Skill/Command（非 Hub managed plugin@cc-partner）进 `<data_dir>/portable-store/`，软链到各家 native 根。MCP 不进该仓库：各家配置 native leaf，跨 Agent 走已有 Pull。Grok 卸下只拆 `~/.grok/...`；若 Claude 仍附加 Skill/Command，盘点标仍经 Claude 路径加载。Plugin 不进该仓库，仍是 viewing 开关。磁盘相对 Hub 账本漂移时，「确认当前版本」只 upsert viewing 的 materialization 哈希，不写该 Agent 文件。遗留 `portable-store/mcp/*.json` 不自动删除、也不再投影。

### 5.6 HeadlessCompletion — Prompt 优化

新能力，不塞进 Runtime。Settings 增加优化用 provider，默认 `claude`。

- Grok：`grok -p <prompt> --output-format json`（或 `streaming-json`，实现时选定一种并写死），cwd = 当前项目根
- Gemini：`gemini -p <prompt>`；若无法稳定解析出单段优化正文，该 provider 在设置里不可选，reason = `headless_unstructured`
- 输出合同不变：需求方视角、不澄清提问、按设置语种生成
- Github 热门解说本轮仍走 Claude CLI，不扩到 Grok/Gemini（那不是「按 agent 适配用户会话」的面，是内部解说引擎）

### 5.7 编排器 / Attention / Settings

- 实验候选与 adapter catalog 读身份目录；禁止写死 `claude + (opencode 或 codex)`
- Settings / Workbench catalog 去掉 `provider === 'openCodeVisible'` 硬编码，改为看 catalog 的 `bridge` 能力位（仅 OpenCode 具备）
- Hub Attention、runtime Attention 跟新 target / provider 走，不新造 Inbox 动作
- Claude status 文件对账保持 Claude 专属；Grok / Gemini 该能力 **缺席**，不伪造 Busy/Idle

## 6. 支持矩阵（V1 交付定义）

「支持 / scan-only / residual / unavailable / 缺席」都是交付物：UI 看得到、点得开、有稳定原因文案。

| 能力 | Grok | Gemini |
|------|------|--------|
| Hub 切换器出现 | 必须 | 必须 |
| 可见终端启动 / Fresh resume | 支持 | 支持（未安装则 unavailable） |
| 编排器自动完成 | Manual | Manual |
| 会话搜索 / preview / resume | 支持 | 支持（hash 回退见 5.2） |
| 自动标题 | 支持 | 支持 |
| Token / 会话用量 | `signals.json` | 字段稳则支持，否则 unavailable |
| Prompt 历史采集 + 同步 | 支持 | 支持 |
| 指令扫描 | 支持 | 支持 |
| 指令写入 adapted/exclusive | `.grok/rules/cc-partner.*.md` | `GEMINI.md` 或 `.gemini/cc-partner.*.md`（单一落点） |
| 指令 common 单独文件 | 不写（复用 AGENTS.md/CLAUDE.md） | 写 `GEMINI.md` |
| Skill / Command 扫描 | 支持 | 支持 |
| Skill / Command 投影 | evidence 齐则写，否则 scan-only | 同左 |
| MCP 扫描 / patch | TOML | JSON |
| Plugin 互拷 / 跨 Agent 翻译 | residual | residual |
| Plugin 启用标记 | 跟 viewing Agent；不继承 Claude `enabledPlugins` | 同左 |
| portable-store Skill/Command 附加 | scan + 兼容提示；apply blocked（无 L3） | 同左 |
| MCP native leaf / Pull | 盘点 + 该 Agent 配置 leaf；不 migrate/attach/detach/destroyStore | 同左 |
| 确认当前版本（漂移账本） | 可 apply；只写 Hub materialization，不写该 Agent 文件 | 同左 |
| Prompt 优化 | 接 headless JSON | 仅结构化输出稳定时开放 |
| Claude 式 status 文件 | 缺席 | 缺席 |
| OpenCode 式 runtime bridge | 不做 | 不做 |

## 7. 数据流与错误

```
身份目录
  ├─ AgentAdapter registry     → 终端 / 编排器
  ├─ SessionHistory registry   → ⌘K / 自动标题
  ├─ UsageSource registry      → ledger / Token 统计
  ├─ HistoryCollector registry → Prompt 历史 / 同步
  ├─ AssetAdapter registry     → Hub / 跨 Agent / 投影
  └─ HeadlessCompletion        → Prompt 优化
```

错误：

- 未知 agent / provider / source → 校验错误，不回退
- CLI 不在 PATH → unavailable，功能面保留入口
- 磁盘格式漂移 / 截断预算 → 沿用现有 SessionSearchDiagnostics reason token
- Hub 写无 evidence → scan-only / blocked，与现有 Gate B 门禁相同
- 跨 Agent 不可移植 → residualReason，不静默跳过且不标成功
- 用量缺失 → null / 「未提供」

LAN：新 Hub target 与新 history source 不新增鉴权。旧 peer 管理不了新 target 时返回既有 unsupported / decoder 错误。

## 8. 测试与验收

1. catalog parse：五 `AgentId`、`genericTerminal` 无 Hub、未知 token 失败。
2. 现有 Claude / Codex / OpenCode：Runtime、搜索、历史、用量、Hub 扫描 characterization 不变。
3. Grok SessionHistory：用 fixture `summary.json` + `updates.jsonl` 测 cwd 过滤、标题、resume args。
4. Grok UsageSource：`signals.json` fixture；缺失文件返回 None。
5. Grok HistoryCollector：只入库用户句；id 前缀 `grok:`。
6. Grok AssetAdapter：common 不写 AGENTS.md；adapted/exclusive 只写 `.grok/rules/cc-partner.*.md`；不写 `~/.claude/**`。
7. Gemini SessionHistory：hash 命中与 tmp 枚举回退两条路径。
8. Gemini HistoryCollector：`gemini:` 前缀；用户句过滤。
9. Gemini AssetAdapter：common 写 GEMINI.md；adapted/exclusive 单一落点；不写 AGENTS.md。
10. 跨 Agent：目的地含 grok/gemini；Plugin residual；Grok common skip（AGENTS.md 已存在）。
11. 前端：搜索 tab、历史筛选、Hub agent 切换、Token provider、Settings catalog 均无字面量三元组。
12. Headless：Grok JSON 路径单测；Gemini 不稳时设置项不可选。
13. 文档：`docs/prd.md`、根/`web`/`src-tauri` AGENTS.md 组件与身份清单、support-manifest 五 target。
14. L3 真机（Grok 本机已装 / Gemini 本机可能未装）保持 NOT VERIFIED，不得用 L2 升格。

## 9. 施工次序（仍属同一 V1）

1. 身份目录 + 扩展全部写死三家的类型与 decoder。
2. 抽出 SessionHistory / UsageSource / HistoryCollector / HeadlessCompletion；迁现有三家。
3. Grok：Runtime + 搜索 + 标题 + 用量 + Prompt 历史 + Hub adapter + headless。
4. Gemini：同上，Usage / headless 按证据降级。
5. 跨 Agent 目的地与 residual 单测。
6. 前端全部改读 catalog；PRD / AGENTS.md / support-manifest 落地。

可按 1→2 串行，3 与 4 在目录稳定后并行。不得在第 1 步完成前把新枚举只加进某一个功能面。

## 10. Spec 自审

- 无「以后再说」的产品面：Hub、Prompt 历史、Token 统计、优化、编排器均有合同。
- Runtime 与 Hub 仍是两套 trait，避免 God Adapter。
- Grok common 不写 AGENTS.md，与「必须进 Hub」不矛盾：Hub 有切换器、扫描和对账，写入策略按文件共享关系诚实。
- Gemini adapted/exclusive 落点在实现对照源码后锁定一种，禁止双写。
- Antigravity 不混入 `gemini`。
- 本 spec 覆盖面适合拆成一个带并行 workstream 的实现计划，而不是多份互相打架的设计。
