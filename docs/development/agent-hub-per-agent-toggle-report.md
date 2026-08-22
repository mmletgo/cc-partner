# Agent Hub：Skill / Command / MCP / Plugin 分 Agent 启停现状与独立管理可行性

> 日期：2026-08-22  
> 范围：七个 Hub 身份（Claude / Codex / OpenCode / Grok / Gemini / Cursor / Pi）× 四类 portable 资产。  
> 依据：`runtime-discovery.json`、`plugin_enablement.rs`、`supports_direct_local_action`、`portable_store/actions.rs`、`adapt-new-agent.md` §3.9–§3.13。代码为准；手册 L3 表述与代码不一致处已标明。

本文回答两件事：

1. 现在每种资产在「自身 / 公用 / 借用」下，运行时能不能单独开、Hub 能不能单独关。  
2. 若要「只关当前 Agent、不动所有者」，哪些已经能做、哪些缺执行器、哪些和 CLI 加载模型冲突。

---

## 1. 术语

| 身份 | 判定 | 典型路径 |
|------|------|----------|
| **自身** | `ownedBy` = 当前 Agent，且 `originKind` 为 `native`；Codex 的 `~/.agents/skills` 是 `legacyStandalone` + `ownedBy=codex`，对 Codex 也算自身 | `~/.claude/skills`、`~/.grok/installed-plugins`、`~/.codex/config.toml` 的 MCP |
| **公用** | `ownedBy=sharedAgents`（`~/.agents`）；或 Hub **仓库**真树 `ownedBy=portableStore` | `~/.agents/skills`；`<data_dir>/portable-store/skills/…` |
| **借用** | `originKind=compatibility`，或 `ownedBy` 是另一家 Hub Agent | Grok 扫到的 Claude plugin cache / Claude `mcpServers`；Cursor 扫到的 Claude/Codex skills |

「单独开启关闭」在本文里只指：**改当前查看 Agent 的加载策略，不改所有者磁盘、不让其他 Agent 一起变。**

Hub 实际动作和这个定义并不总一致：

- Skill/Command **仓库附加/卸下**：按设计是 viewing 软链，属于单独管理。  
- Plugin **启用/禁用**：按设计写 viewing 开关；卸载仍改所有者。  
- 未进仓库的 Skill/Command/MCP **启用/禁用/卸载**：走 **所有者**（`mutation_target_for_origin`）。从 Grok 列表去关 Claude MCP，会改 Claude 配置，**不是**单独管理。

---

## 2. 总原则（当前实现）

1. **Skill / Command 进仓库后**：本机一份真树 + 各 Agent native 根上的软链。Hub 用「附加 / 从此 Agent 卸下」，七家都能做（纯文件，不 spawn CLI）。仓库页每家一枚芯片。  
2. **Skill / Command 未进仓库**：Claude/Codex 仍可能 MOVE 到 disabled 目录（这是该 Agent 自己的目录，但仍不是「借用隔离」）。Codex **没有**独立 command enable 语义。  
3. **Plugin 永不进仓库**。启用态只读 **当前查看 Agent** 的开关文件。关掉 Claude ≠ 关掉 Grok。Hub **写**开关目前只有 Claude（`claude plugin enable/disable`）和 Codex（改 `config.toml`）。  
4. **MCP 永不进仓库**。每家自己的配置 leaf 本来就是分开的文件。跨 Agent 复制走 Pull。Hub **写** leaf 目前只有 Claude（挪到 disabled 快照）和 Codex（翻 `enabled`）。  
5. **Grok 是唯一会列出 Claude plugin registry 和 Claude MCP 的借用方。** Cursor/Gemini/Pi/OpenCode **不会**为了对称去扫 Claude plugin cache。  
6. **support-manifest**：Claude / Codex 的 portable 写 + activate/deactivate = `supported`（需 CLI 探测成功）。其余五家 = `blocked`。Skill/Command 仓库动作在代码里对七家都开放（与手册「无 L3 不得 apply store」不完全一致，以代码为准）。  
7. **打包 GUI PATH** 若找不到 `claude`，Claude 的 Plugin/MCP Hub 按钮也会全部消失（probe `cli_version_unknown`）。Skill/Command 仓库动作不受影响。

---

## 3. 谁会加载什么（盘点范围）

只统计库存会扫到的根，不是「磁盘上有」。

| Agent | 自身 Skill | 自身 Command | 自身 Plugin | 自身 MCP | 借用 Skill | 借用 Command | 借用 Plugin | 借用 MCP | 公用 `~/.agents` |
|-------|------------|--------------|-------------|----------|------------|--------------|-------------|----------|------------------|
| Claude | 有 | 有 | 有 | 有 | 无 | 无 | 无 | 无 | **不扫** |
| Codex | 有；`~/.agents` 算自身 legacy | 有 | 有 | 有 | 无 | 无 | 无 | 无 | 对 Codex 不是借用 |
| OpenCode | 有 | 有 | 有 | 有（发现表外：`opencode.json(c)` `mcpServers`） | Claude（可用 env 整根关掉） | 无 | 无 | 无 | Skill，env 可整根关 |
| Grok | 有 | 有 | 有（`plugins` + `installed-plugins`） | 有（`config.toml`） | Claude | `~/.agents/commands` | **Claude registry**（唯一） | **Claude `mcpServers`**（唯一） | Skill + Command |
| Gemini | 有 | 有 | **无** | 有（`settings.json`） | 无 Claude | 无 | 无 | 无 | 仅 Skill |
| Cursor | 有 | 有 | **无** | 有（`mcp.json`） | Claude + Codex | 无 | 无 | 无 | Skill（user/project） |
| Pi | 有 | **无** | **无** | **无** | Claude/Codex **仅当** settings `skills` 点名该路径 | 无 | 无 | 无 | Skill 无条件 |

门闩：

- OpenCode：`OPENCODE_DISABLE_EXTERNAL_SKILLS` 关全部外部 Skill；`OPENCODE_DISABLE_CLAUDE_CODE(_SKILLS)` **只关** `.claude`，不能拿来关 `.agents`。这是**整根**开关，不是逐条。  
- Pi：Claude/Codex 目录必须在 `settings.json` 的 `skills` 数组里**点名路径**；点名 Claude 不会自动打开 Codex。这也是**根级**，不是逐条。

---

## 4. Hub 按钮现状（按资产）

图例：

- **单独**：按钮只改当前 Agent。  
- **全局/所有者**：按钮改所有者磁盘，借用者一起变。  
- **无按钮**：`canEnable`/`canDisable`/`canAttach`/`canDetach` 均为 false。  
- **运行时已有开关**：CLI 配置能表达启停，Hub 没接写。

### 4.1 Skill

| Agent | 自身（未进仓库） | 自身（已进仓库） | 公用 `~/.agents` | 借用其他 Agent |
|-------|------------------|------------------|------------------|----------------|
| Claude | Hub 不再给 enable/disable；主路径是 **迁入仓库**。未迁入时无逐条「关」 | **附加/卸下** 单独（软链） | 不扫 | 无 |
| Codex | 同上；`~/.agents` 可迁入仓库 | 附加/卸下 单独 | 对 Codex 即自身 legacy | 无 |
| OpenCode / Grok / Gemini / Cursor / Pi | 无 enable/disable；有 native 根则可 **附加/卸下** 仓库项 | 附加/卸下 单独 | 库存为借用；**无迁入**；仓库芯片为暗淡只读 | 暗淡芯片只读；「卸下」若出现会拆 **源 Agent 软链**（让所有读这条链的 Agent 一起卸） |

Skill 要「Grok 不用、Claude 继续用」：正确动作是 **Grok 不附加 / 从 Grok 卸下自己的软链**，而不是 disable Claude 目录。若 Grok 只是扫 Claude 目录、自己没有软链（`loadedViaOtherPath`），芯片是暗淡只读，Hub **没有**「Grok 忽略这条 Claude skill」的逐条开关。

### 4.2 Command

| Agent | 自身 | 仓库 | 公用 / 借用 |
|-------|------|------|-------------|
| Claude | 有 disabled 路径语义；Hub 主路径仍是迁入仓库 + 附加/卸下 | 附加/卸下 单独 | 不扫 `~/.agents` |
| Codex | **无**独立 command enable 语义（`enable_semantics_supported` 对 Codex Command 为 false） | 附加/卸下 单独 | `~/.agents/commands` 未在 Codex 发现表（Codex 只把 `~/.agents/skills` 标 legacy） |
| OpenCode | 有 disabled 路径语义，但无 direct-local enable；靠仓库附加/卸下 | 附加/卸下 单独 | 不借用别人的 command |
| Grok | 同 OpenCode 执行器；靠仓库 | 附加/卸下 单独 | 借用 `~/.agents/commands`：无逐条独立关 |
| Gemini / Cursor | 有 native command 根；靠仓库 | 附加/卸下 单独 | 不借用 command |
| Pi | **缺席** | 无 | 无 |

### 4.3 Plugin

运行时启用态（扫描，不写盘）：

| Viewing | 自身未登记 | 借用未登记 | Hub 能否写开关 |
|---------|------------|------------|----------------|
| Claude | 已安装且 `enabledPlugins` 无键 → **开** | （Claude 不借用） | **能**（`claude plugin enable/disable`，需 CLI 探测成功） |
| Codex | 表非空且未登记 → **关** | 不扫 Claude registry | **能**（改 `[plugins."id@market"] enabled`） |
| Grok | `enabled` 非空当白名单 | 只认自己的 `disabled`；**不继承** Claude `enabledPlugins` | **不能**（无执行器；禁止 remap 到 `claude plugin`；也禁止未认证就跑 `grok plugin disable`，短名会和 Claude cache 撞名） |
| OpenCode / Gemini / Cursor / Pi | 目录存在即开；后三家基本无 plugin 根 | 不扫 Claude registry | **不能** |

借用 Plugin 只出现在 **Grok** 列表。Hub 策略：Enable/Disable **目标是 Grok**，但 Grok 不在 allowlist，所以 **无按钮**。Uninstall 仍指向 Claude 磁盘，借用行可能出现「卸载」（改所有者）。

### 4.4 MCP

| Agent | 自身 MCP 文件 | 自身 Hub 启停 | 借用 Claude MCP |
|-------|---------------|---------------|-----------------|
| Claude | `~/.claude.json` / `.mcp.json` 的 `mcpServers` | **能**（Disable 把 leaf 挪到 `claude-assets/disabled/mcp/`，Enable 再写回） | 无 |
| Codex | `config.toml` `[mcp_servers]` | **能**（只翻 `enabled`，不删表） | 无 |
| OpenCode | `opencode.json(c)` `mcpServers` | **不能** | 无 |
| Grok | `~/.grok/config.toml` | **不能**（文件是独立的，Hub 没接） | **会列出**；启停门闩走 **所有者 Claude**。若按钮出现，点下去会改 Claude 配置 → **全局**，违反「单独」。当前因 Grok 列表 + 能力门闩，通常 **无按钮** |
| Gemini | `settings.json` | **不能** | 无 |
| Cursor | `mcp.json` `mcpServers` | **不能** | 无 |
| Pi | **缺席** | — | 无 |

MCP 跨 Agent 的「单独用」官方路径是 **Pull 一份到当前 Agent 的 leaf**，不是给借用行做 viewing 开关。

---

## 5. 总表（运行时单独 vs Hub 单独）

「运行时单独」= 这个 CLI 加载时能否在不改别人配置的前提下不加载该项。  
「Hub 单独」= 库存页能否发出只影响当前 Agent 的动作。

### 5.1 自身资产

| | Skill | Command | Plugin | MCP |
|---|--------|---------|--------|-----|
| Claude | 运行时：disabled 目录 / 卸软链。Hub：仓库附加/卸下 **单独**；未进仓库无 enable 按钮 | 同 Skill（有 disabled 路径） | 运行时+Hub：**单独**（`enabledPlugins`） | 运行时+Hub：**单独**（自家 json） |
| Codex | 同 Claude | 运行时无 command enable。Hub：仅仓库附加/卸下 | 运行时+Hub：**单独**（toml enabled） | 运行时+Hub：**单独**（toml `enabled`） |
| OpenCode | 仓库附加/卸下 **单独** | 仓库附加/卸下 **单独** | 运行时目录即开。Hub **无** | 运行时自家 json。Hub **无** |
| Grok | 仓库附加/卸下 **单独** | 仓库附加/卸下 **单独** | 运行时 toml 黑白名单 **已独立**。Hub **无写** | 运行时自家 toml **已独立**。Hub **无写** |
| Gemini | 仓库附加/卸下 **单独** | 仓库附加/卸下 **单独** | 缺席 | 运行时自家 settings。Hub **无** |
| Cursor | 仓库附加/卸下 **单独** | 仓库附加/卸下 **单独** | 缺席 | 运行时自家 mcp.json。Hub **无** |
| Pi | 仓库附加/卸下 **单独** | 缺席 | 缺席 | 缺席 |

### 5.2 公用 `~/.agents`

| Agent | 是否加载 | 逐条单独关（运行时） | Hub |
|-------|----------|----------------------|-----|
| Claude | 不扫 | — | — |
| Codex | Skill 当自身 legacy | 迁入仓库后按 Codex 软链关 | 可迁入 / 附加 / 卸下 |
| OpenCode | Skill，可整根 env 关 | **不能逐条**；只能 `OPENCODE_DISABLE_EXTERNAL_SKILLS` | 无迁入；芯片暗淡 |
| Grok | Skill + Command | **不能逐条** | 芯片暗淡 |
| Gemini / Cursor / Pi | Skill | Pi/OpenCode 仅根级门闩 | 芯片暗淡 |

### 5.3 借用其他 Agent

| 借用方 | 借用内容 | 运行时是否跟所有者开关 | Hub 单独关 |
|--------|----------|------------------------|------------|
| Grok | Claude Skill | 跟 Claude 目录是否存在/是否被加载，无 Grok 逐条 ignore | 否（暗淡芯片；卸下会拆源链） |
| Grok | Claude Plugin | **不跟** Claude `enabledPlugins`；可被 Grok `disabled` 关掉 | **否**（读得到、写不了） |
| Grok | Claude MCP | 跟 Claude 配置 leaf；Grok 无自己的「不加载这条 Claude MCP」清单 | **否**（且禁止做成 remap Claude） |
| Cursor | Claude + Codex Skill | 跟源目录 | 否 |
| Gemini | 仅 `.agents` Skill | 跟目录 | 否 |
| OpenCode | Claude Skill | 整根 env，非逐条 | 否 |
| Pi | 点名的 Claude/Codex Skill 根 | 根级 settings | 否 |
| Claude / Codex | 不借用别人 plugin/MCP/skill | — | — |

---

## 6. 独立管理可不可做

按「只改 viewing、不 remap 别人 CLI、尽量不发明官方没有的配置」排序。

### 6.1 已经等于单独管理（不必新模型）

1. **七家 Skill/Command 进仓库后的附加/卸下。** 这就是「这个 Agent 用不用这份真树」。缺口是：用户没迁入仓库时，列表里没有等价开关；借用且没有自己软链时只有暗淡芯片。  
2. **Claude / Codex 自身 Plugin、自身 MCP。** 已经是 viewing/native leaf。前提是 CLI probe 成功（GUI PATH 要找得到二进制）。  
3. **Grok 自身 Plugin 的运行时语义。** `[plugins] disabled` 已经能单独关借用包和 native 包；只差 Hub 去写这份 toml。

### 6.2 可做、且与现有合同一致（建议优先）

| 项 | 做法 | 风险 | 工作量 |
|----|------|------|--------|
| **Grok 自身 + 借用 Plugin 的 Hub 启用/禁用** | 只 patch `~/.grok/config.toml` 的 `[plugins].enabled` / `.disabled`，对标 Codex 的 toml patch。**禁止** spawn `grok plugin` / `claude plugin` | 短名碰撞已有文档警告，必须用 marketplace 限定 id 写入 `disabled` | 中：执行器 + allowlist 扩到 Grok Plugin Enable/Disable + 单测 + 不必等 L3 CLI（与仓库软链一样是纯文件）。若坚持手册「无 L3 不写原生文件」，则需先 L3 或把「写 grok config.toml」做成与 store 同类例外 |
| **Grok / Gemini / Cursor / OpenCode 自身 MCP 启停** | 各写自己的 leaf：`enabled` 字段或等价语义（Codex 已是翻 flag；Claude 是挪快照，不必抄给别人） | 要核对各家配置 schema（Grok toml、Gemini settings、Cursor mcp.json、OpenCode mcpServers）是否真有 per-server enabled；没有则只能删 leaf（那是卸载不是关） | 中，每家一份 patcher |
| **借用 Skill 的「在此 Agent 卸下源软链」收紧** | 暗淡芯片保持只读；只有 **当前 Agent 自己 native 根上的软链** 才能卸。禁止从 Grok 卸 Claude 的链 | 改现有 `can_detach` 对 `borrowed_store_runtime` 的行为 | 小，但是行为变化，要改测试 |

### 6.3 可做、但要新产品语义

| 项 | 为什么难 | 可能做法 |
|----|----------|----------|
| **借用 Claude MCP 在 Grok 上单独关** | Grok 扫描的是 Claude 的同一份 `mcpServers`。官方没有「Grok 忽略某条 Claude MCP」字段。Hub 若提供按钮，按现有 MCP 门闩会写 Claude → 全局关闭 | 1）不要在 Grok 列表提供借用 MCP 的 enable/disable；只提供「Pull 到 Grok 自己的 config」。2）若 Grok CLI 支持 mcp deny/allow，再接到 viewing 配置。3）不要发明 Hub 私有黑名单却不进 Grok 运行时 |
| **借用 Skill 逐条忽略（无软链）** | OpenCode/Pi 只有整根门闩；Grok/Cursor/Gemini 官方是「扫到就加载」 | 逐条忽略需要 CLI 支持，或接受 Hub 只改「是否在自己的 native 根建链」、不保证停止扫兼容根。后者 **不能** 让 Grok 真正不加载 Claude skill |
| **OpenCode / Cursor / Pi Plugin** | 发现表里后两家没有 plugin 根；OpenCode 是目录即开、无 Hub 可读开关 | 先对照官方是否有 per-plugin disable；没有就不要做 Hub 开关 |

### 6.4 明确不要做

1. 从 Grok/OpenCode 列表 remap 到 `claude plugin disable`。  
2. 从 Grok 列表 disable 借用 MCP 却去改 `~/.claude.json`。  
3. 把 MCP/Plugin 塞进 portable-store（合同禁止；MCP 含凭据 leaf，Plugin 是 viewing 开关）。  
4. 为对称让 Cursor/Gemini/Pi 列出 Claude plugin cache。  
5. 把「关掉 Claude plugin」宣传成关掉所有 Agent。

---

## 7. 建议落地顺序（若要做独立管理）

1. **保底体验**：Claude/Codex Plugin+MCP 按钮依赖 CLI 探测；GUI PATH 必须能找到 `~/.local/bin`（已按此修 probe）。  
2. **Grok Plugin 写 `disabled`/`enabled`**：这是用户问题的直接答案——Grok 借用 Claude plugin 时，运行时已经可以单独关，Hub 补上按钮即可。  
3. **自身 MCP 写各家 leaf**（Grok → Gemini → Cursor → OpenCode），按钮文案必须是「只改当前 Agent」。  
4. **借用 MCP 不提供启停**，只提供 Pull / 「在所有者中打开」。  
5. **Skill 借用**：继续用仓库芯片；把「卸源链」从借用行拿掉，避免误伤 Claude。逐条「忽略兼容根里的某一条」等官方开关再做。

---

## 8. 结论

- **Skill/Command**：单独管理的正路已经存在（仓库附加/卸下，七家）。没迁入仓库、或只是扫别人的目录，Hub 就不能逐条单独关。  
- **Plugin**：单独管理的语义已经存在（viewing 开关）。Claude/Codex Hub 能写；**Grok 运行时能写、Hub 还不能写**；其余家要么没有 plugin，要么目录即开。借用 Claude plugin **只有 Grok 会列**，且扫描已做到不继承 Claude 的关。  
- **MCP**：自身配置本来就是分文件的，单独管理对自身成立，Hub 只接了 Claude/Codex。借用 Claude MCP **只有 Grok 会列**，目前 **没有** 合法的「只关 Grok」写路径；做成按钮极易变成关 Claude。

若只做一件事让「Grok 借用的 plugin 可单独开关」成立：给 Grok 接 `config.toml` `[plugins]` 的 Enable/Disable 执行器，不要调任何 CLI。

落地合同与分期见：

- 设计：[../superpowers/specs/2026-08-22-agent-hub-independent-toggle-design.md](../superpowers/specs/2026-08-22-agent-hub-independent-toggle-design.md)
- 实施计划：[../superpowers/plans/2026-08-22-agent-hub-independent-toggle.md](../superpowers/plans/2026-08-22-agent-hub-independent-toggle.md)
