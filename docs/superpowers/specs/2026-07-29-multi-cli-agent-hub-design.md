# 多 CLI Agent Hub 设计

- 日期：2026-07-29
- 状态：已确认
- 目标 CLI：Claude Code、Codex CLI、OpenCode
- 适用范围：用户级配置，以及用户在 Workbench 中登记并显式启用的项目

## 1. 问题

cc-partner 当前的 Agent 相关能力以 Claude Code 为中心：

- user 级 `~/.claude/CLAUDE.md` 有独立编辑、SQLite 对账、局域网主动推送与 GitHub 推送流程；
- Claude Code skills、commands、plugins、MCP 有本机扫描、启停、卸载和局域网选择性拉取；
- Workbench/Orchestrator 已有 provider-neutral adapter 雏形，并已支持可见 Claude 与 Codex Runner；
- OpenCode 尚无资产适配器与运行时适配器。

这套实现不能通过增加几个复制目录的分支扩展为多 CLI：

1. `CLAUDE.md` 与 `AGENTS.md` 并非整文件等价。有些内容是项目事实和共同约定，有些内容依赖 CLI 的指令发现顺序、工具名、配置路径或运行时语义。
2. 同一目录需要同时服务 Codex 与 OpenCode，但二者都以 `AGENTS.md` 为主要项目规则入口，不能让两种专属内容相互污染。
3. Skill 大体可移植，Plugin 却不是统一运行时。Claude/Codex 的 bundle 与 OpenCode 的 JS/TS/npm plugin 不可按目录名直接互换。
4. MCP、Agent、Command 的模型接近但字段、存储位置和调用方式不同。
5. 用户会直接在任一 CLI 的原生文件中编辑，因此 cc-partner 不能只做单向生成，也不能让多个 watcher 反复覆盖彼此。
6. 用户要求同一机器上的多个 CLI 在 sidecar 后台自动同步；跨设备则由源设备手动选择目标并推送。

## 2. 已确认的产品决策

1. cc-partner 的 Canonical Hub 是唯一逻辑真相源；各 CLI 文件是可编辑投影。
2. 同机同步由独立后端 sidecar 持续扫描、监听和对账，GUI 关闭后仍运行。
3. 支持用户级和 Workbench 登记项目；项目根和任意子目录指令文件均纳入。
4. 每个项目首次写入前必须展示精确预览并取得一次性启用；启用后允许后台原子写入，但绝不自动 `git add`、commit 或 push 项目仓库。
5. 指令采用“共享正文 + 确定性适配块 + CLI 专属块”，不做整文件盲目翻译。
6. 子目录指令文件首次仅发现一个来源时，正文默认全部共享；只有用户显式标记或已有来源可证明的块才是 CLI 专属。
7. Plugin 先拆成 capability，再分别投影；无法安全转换的 hook、运行时代码和 target 扩展保留为目标专属，并显示 partial/source-only 诊断。
8. MCP 环境变量、HTTP headers、URL 中的凭据及其他配置值按原文保存和传输，不再对局域网或 Git snapshot 做内容脱敏。
9. GitHub 私有仓库保存完整、明文 Hub snapshot；后台可自动 push，但远端 snapshot 进入本机 Hub 前必须由用户确认。
10. 局域网不提供目标侧任意 pull 产品流程；源设备选择一个或多个目标后主动 push。
11. 冲突使用上次投影作为 base 做三方合并；不相交变更自动合并，同一逻辑块或同一配置键冲突时暂停相关资产并进入 Attention。
12. 首版不同步模型选择、provider 账号、审批/权限策略、会话、认证状态或 CLI 历史。

## 3. 目标

1. 在一台机器上自动收敛 Claude Code、Codex CLI、OpenCode 的用户级和项目级 Agent 资产。
2. 保留用户使用原生编辑器或 CLI 修改文件的习惯，所有原生视图均可作为变更入口。
3. 对普通项目说明原样同步；对已知 CLI 语义做确定性适配；对不可移植部分诚实降级。
4. 使用一个统一版本链、冲突模型、文件投影器、LAN push 和 Git snapshot，而不是为每个 CLI 再造一套同步引擎。
5. 将现有 Claude-only 资产页和 CLAUDE.md 页迁移到统一 Agent CLI 资产工作区。
6. 补齐 OpenCode 可见 Workbench/Orchestrator Runner，并把既有 Codex adapter 纳入稳定注册表。
7. 保证任何失败都不会静默覆盖用户文件，也不会把 partial 状态显示为已完全同步。

## 4. 非目标

- 不把 Claude、Codex、OpenCode 的整个 home 目录做镜像。
- 不同步 API key 登录状态、OAuth token、session transcript、历史对话或模型缓存。
- 不统一三种 CLI 的全量 config；只对本 spec 明确列出的 MCP、Agent、Command 与 Plugin 激活片段做语义 patch。
- 不用大模型在后台自由改写任意指令或可执行代码。
- 不把 Claude/Codex hook 自动翻译成 OpenCode JS/TS plugin，也不把 OpenCode plugin 源码转换为其他运行时。
- 不保证已启动的 CLI session 热加载新指令；目标 adapter 必须显示“即时生效 / 新 session 生效 / 重启生效”。
- 不自动修改或提交用户项目的 Git 状态。
- 不新增 LAN 身份鉴权、设备权限矩阵或 capability token。协议 capability 只用于版本协商，不表示身份可信。

## 5. 总体架构

```text
CLI 原生文件 / 配置 / registry
       │
       ▼
Target Scanner + File Watcher
       │
       ▼
Three-way Reconciler ───冲突───► Attention
       │
       ▼
Canonical Hub
  ├─ SQLite 事务元数据
  └─ Content-addressed objects
       │
       ▼
Projection Scheduler
  ├─ Claude Asset Adapter
  ├─ Codex Asset Adapter
  └─ OpenCode Asset Adapter
       │
       ├────────► 同机原生视图
       ├────────► 手动 LAN source push
       └────────► Git 完整 snapshot 自动 push

独立边界：
Agent Runtime Registry
  ├─ claudeCodeVisible
  ├─ codexVisible
  └─ openCodeVisible
```

### 5.1 组件边界

`AgentHubStore`
: 管理作用域、逻辑资产、版本、变体、投影状态和冲突；只依赖数据库写入许可与对象库。

`ObjectStore`
: 以 SHA-256 保存不可变文件字节和目录清单；不理解 CLI 语义。

`InstructionCompiler`
: 管理 Markdown 块来源、共享/适配/专属策略，以及目标文件渲染和反向变更映射。

`AssetAdapter`
: 每个 CLI 一个实现，负责 probe、scan、normalize、render、activate、deactivate 和生效提示；不负责版本或冲突。

`PluginDecomposer`
: 将 plugin package 拆成 Skill、MCP、Command、Agent、Hook 和 residual runtime payload；不直接写目标文件。

`Reconciler`
: 使用 materialization base、Hub current 与外部 current 做三方合并，并生成 revision 或 conflict。

`ProjectionScheduler`
: 持久化文件写入 job，执行 precondition check、暂存、原子替换与 crash recovery。

`AgentHubLanPush`
: 源驱动、用户触发的 Hub-to-Hub 推送；不提供产品层 target-side pull。

`AgentHubGitSnapshot`
: 确定性导出完整 Hub 并自动 push；远端内容只在显式 preview/confirm 后导入。

`AgentRuntimeAdapter`
: 只负责启动、resume、completion/runtime 事件，不依赖 AssetAdapter 的内部存储。

## 6. Canonical 数据模型

### 6.1 作用域

`ScopeNode` 使用稳定 ID 表示：

- `user`：当前 OS 用户；
- `project`：Workbench 项目；
- `directory`：项目内相对目录。

项目路径不作为跨设备身份。项目节点保存：

- cc-partner 稳定 `hubProjectId`；
- owning device 的本地 Workbench project ID；
- 可选的规范化 Git remote fingerprint；
- 本机绝对路径只保存在本机映射表，不进入可移植 snapshot。

目录节点只保存项目 ID 与规范化相对路径。根目录使用空相对路径。

### 6.2 逻辑资产

`LogicalAsset` 至少包含：

- `id`
- `scopeId`
- `kind`：`instruction | skill | command | agent | mcp | plugin`
- `logicalKey`
- `displayName`
- `policy`：`shared | adapted | targetOnly`
- `currentRevisionId`
- `deletedAt`

同名资产在不同作用域是不同 LogicalAsset。同一作用域内以 `kind + logicalKey` 唯一。

### 6.3 版本

每次被接受的本地编辑、UI 编辑、LAN push 或 Git import 都生成不可变 `Revision`：

- `id`
- `assetId`
- `parentRevisionId`
- `originKind`：`filesystem | ui | lan | git | migration`
- `originTarget`：`claude | codex | opencode | null`
- `originDeviceId`
- `payloadHash` 或 `treeManifestHash`
- `createdAt`

Revision 是线性本地提交链。并发输入先通过三方合并收敛；无法收敛时不推进 current revision。

### 6.4 Target variant

每个资产可保存：

- 一份 common payload；
- Claude target extension；
- Codex target extension；
- OpenCode target extension。

Target extension 只能表达对应 adapter 已声明的字段，未知字段以原始 target payload 保存，不强行塞进 common schema。

### 6.5 Materialization

每个 `asset + target + localScopeMapping` 有一条 materialization：

- 原生目标路径或 registry key；
- 上次成功投影的 revision；
- base 内容/目录清单；
- base block map；
- rendered hash；
- 最后观察到的外部 hash；
- adapter ID、adapter schema version、CLI version；
- 生效方式；
- 状态：`synced | pending | drift | partial | conflict | blocked | unsupported`；
- 最近 job/error。

### 6.6 Conflict

Conflict 保存：

- base revision/payload；
- Hub current revision/payload；
- 外部 target 与 payload；
- 冲突块、文件路径或配置键；
- 创建时间和解决状态。

Conflict 只冻结相关资产和受影响 target。其他作用域、资产与目标继续收敛。

## 7. 物理存储

### 7.1 SQLite

新增 Agent Hub 领域表，所有写入经过 sidecar owner 与现有数据库写入许可：

- scopes / project mappings；
- assets / revisions / variants；
- materializations / projection jobs；
- conflicts；
- LAN push ledger；
- Git snapshot state；
- target probe state。

数据库迁移必须幂等。旧版本回滚时保留新表，不自动删除 Hub 数据。

### 7.2 Content-addressed object store

默认位于 `<data_dir>/agent-hub/objects/sha256/`。

- 普通文件按精确字节保存；
- Skill/Plugin 等目录用 `TreeManifest` 保存规范化相对路径、blob hash、文件类型和 executable bit；
- 不跟随指向资产根外的 symlink；这类条目标记 target-only/partial；
- object 写入使用临时文件、校验 hash 后原子 rename；
- 未被 revision、projection job 或 snapshot 引用的 object 才能 GC。

凭据与普通内容一样以明文 blob 保存，不做 field redaction 或加密包装。

### 7.3 Git snapshot

Git 不提交 SQLite 或内部 CAS 布局，而提交确定性、可读、可完整恢复的 snapshot：

```text
agent-hub/
└── devices/<deviceId>/
    ├── snapshot.json
    ├── user/
    │   ├── instructions/
    │   ├── skills/
    │   ├── commands/
    │   ├── agents/
    │   ├── mcp/
    │   └── plugins/
    └── projects/<hubProjectId>/
        ├── project.json
        ├── instructions/<relative-dir>/
        └── assets/
```

每台设备只写自己的 lane，避免两个自动 push 直接改同一 snapshot 文件。后台允许 fetch/rebase 以完成 push，但不会把其他 device lane 导入 Hub。

snapshot 包含完整明文凭据。凭据一旦进入 Git 历史，删除当前文件不等于从历史安全擦除；产品不能承诺可恢复的 secure erase。

## 8. Instruction Compiler

### 8.1 块模型

一个目录对应一个 `InstructionDocument`。文档由有序 `InstructionBlock` 组成：

- 稳定 `blockId`；
- `mode`：`shared | adapted | targetOnly`；
- common Markdown 或 structured intent；
- 显式 target variants；
- 顺序与父 heading 信息。

目标文件中不插入 cc-partner 私有 HTML marker。block ID、上次 byte range 与内容 hash 只保存在 materialization base map。

### 8.2 目标路径

| 作用域 | Claude | Codex | OpenCode |
|---|---|---|---|
| 用户级 | `~/.claude/CLAUDE.md` | `~/.codex/AGENTS.override.md` | `~/.config/opencode/AGENTS.md` |
| 项目任意目录 | `<dir>/CLAUDE.md` | `<dir>/AGENTS.override.md` | `<dir>/AGENTS.md` |

项目目录使用 `AGENTS.override.md` 作为 Codex 投影，是为了让 Codex 在该目录忽略 OpenCode 专属的 `AGENTS.md`；当前 OpenCode 使用 `AGENTS.md`，不会读取 Codex override。

### 8.3 首次纳管

1. 只有一个目标文件存在：整份正文按 shared 导入；随后生成其他已启用目标。
2. 多个目标文件存在且完全相同：导入为同一 shared 文档。
3. 多个目标文件不同：完全相同的 Markdown 块归为 shared；其余块先保留为对应 targetOnly，不通过模型猜测共同意图。
4. 用户可在 preview 中把多个独有块配对为 adapted variants，或提升为 shared。
5. 任何目标文件被覆盖前都必须显示精确 diff；项目还需完成一次性写入启用。

### 8.4 后续外部编辑

Reconciler 以“上次目标投影、当前 Hub、当前目标文件”做三方比较：

- 修改已有 shared 块：更新 common Markdown，并原样投影到其他目标；
- 删除 shared 块：从所有目标删除；
- 修改 adapted 块：只更新本 target variant，不反推 common intent；
- 修改 targetOnly 块：只更新该 target；
- 新块位于来源明确的 shared 区域：继承 shared；
- 新块位于来源明确的 target 区域：继承 targetOnly；
- 块来源、整文件替换或移动无法可靠判断：进入 conflict。

如果文档没有专属块，则走简化的整文档三方合并。典型效果：

```text
src-tauri/CLAUDE.md 的普通说明变化
  → 更新 (project, src-tauri/) 的 shared 文档
  → 同步相同 Markdown 正文到
     src-tauri/AGENTS.override.md
     src-tauri/AGENTS.md
```

父目录内容不展开到子目录。三种 CLI 继续使用各自原生的根到 cwd 指令发现和继承机制。

### 8.5 确定性适配

`adapted` 块分两类：

1. **显式 variants**：用户分别维护 Claude/Codex/OpenCode Markdown。
2. **structured intent**：由内置、版本化 renderer 生成目标文本。

首版内置 `instruction.discovery.before-edit` intent，表达：

- 从项目根到目标目录读取分层指令；
- 每层文件优先级；
- 先使用分层代码地图和定向读取，再扩大搜索；
- Read/Edit 前确认目标目录规则。

它分别渲染为 Claude 的 `CLAUDE.md` 发现措辞、Codex 的 `AGENTS.override.md → AGENTS.md → fallback` 优先级，以及 OpenCode 的 `AGENTS.md` 与 `opencode.json instructions` 语义。

任意自由文本若含 CLI 文件名、CLI 专属工具名、hook event、配置路径或调用语法，只产生 `needsAdaptation` 建议，不做无约束模型重写。用户确认一次 variants 后，后续自动同步这些 variants。

## 9. 资产规范化与目标适配

### 9.1 Skill

Canonical Skill 是一个包含 `SKILL.md` 与 supporting files 的目录树。

| Target | 用户级 | 项目级 |
|---|---|---|
| Claude | `~/.claude/skills/<name>` | `.claude/skills/<name>` |
| Codex | `~/.agents/skills/<name>` | `.agents/skills/<name>` |
| OpenCode | `~/.config/opencode/skills/<name>` | `.opencode/skills/<name>` |

规则：

- `name`、`description` 与 Markdown 正文是 common；
- Codex 的 `agents/openai.yaml`、OpenCode metadata 等进入 target extension；
- supporting files 原字节同步；
- 脚本内容不自动改写。脚本引用专属 executable、工具名或绝对路径时显示 portability diagnostic；
- 名称不满足目标约束时，target adapter 生成稳定 alias，并在 UI 显示 canonical name 与 materialized name。

虽然 OpenCode 能读取 `.claude/skills` 和 `.agents/skills`，仍投影到 `.opencode/skills`，以便清楚区分 OpenCode target variant，避免隐式兼容路径掩盖冲突。

### 9.2 Command

Canonical Command 保存名称、description、prompt template、参数占位语义与 target extension。

- Claude：原生 command Markdown；
- OpenCode：原生 `.opencode/commands/*.md` 或用户级 commands；
- Codex：适配为显式调用 Skill；不伪造不存在的 Claude/OpenCode slash-command 语义。

无法等价映射的 shell interpolation、agent/model override 等字段保留 target extension，并将 Codex 投影标记 partial。

### 9.3 Agent

Canonical Agent 保存名称、description、system/prompt instructions、mode intent 与可移植 tool intent。

- Claude：原生 project/user agent Markdown；
- Codex：生成 agent config file，并通过 `agents.<name>.description/config_file` 做语义 TOML patch；
- OpenCode：原生 project/user agent Markdown。

模型、provider 与权限不做跨目标自动等价推断。已有 target 字段可以保留和回写，但不会从一个 CLI 自动覆盖另一个 CLI 的全局策略。

### 9.4 MCP

Canonical MCP server 使用 typed schema：

- `stdio | http` transport；
- command、args、cwd；
- env；
- URL、static headers、environment-backed headers；
- enable state；
- tool allow/deny 等可移植字段；
- target extensions。

目标 adapter 对 Claude JSON、Codex TOML、OpenCode JSON/JSONC 做语义 patch，只修改由 Hub 管理的 server keys，保留其他配置、顺序与可保留的注释。

MCP 的所有值按原文进入 Hub、LAN payload、Git snapshot 和目标配置。inventory 可以只传 hash/size 等元数据以减少流量，但这不是脱敏；实际 push 和 snapshot 必须完整。

### 9.5 Plugin

Canonical PluginPackage 不是最低同步单位，而是：

```text
PluginPackage
├── package metadata
├── componentRefs[]
│   ├── Skill
│   ├── MCP
│   ├── Command
│   └── Agent
└── residualPayloads[]
    ├── Claude hooks/runtime
    ├── Codex hooks/assets
    └── OpenCode JS/TS/npm runtime
```

分解规则：

- Skill、MCP、Command、Agent 先归一化并独立投影；
- manifest 展示信息可映射时生成目标 manifest；
- hook 只有在事件、输入输出与信任模型存在显式 adapter 时才转换；
- OpenCode JS/TS plugin、custom tool 与 npm package 默认只在 OpenCode 投影；
- Claude/Codex runtime 文件默认只回到来源 target；
- package 中部分 component 成功时状态为 partial，不回滚已可移植 component，也不显示为 full synced。

Codex target 使用 cc-partner 管理的 personal/repo marketplace 与 `.codex-plugin/plugin.json` 结构。若当前 Codex 版本没有稳定的无交互 activation surface，Hub 仍生成并更新 package/marketplace，但状态为 `activationRequired`；不得调用仍标记 under development 的 App Server plugin install API 冒充稳定支持。

OpenCode 原生 JS/TS/npm plugin 只做同 target 同步。跨 target 时只抽取其可识别的 Skill、MCP、Command、Agent，运行时代码保留 source-only。

## 10. 同机自动对账

### 10.1 Sidecar 生命周期

独立后端 owner 启动后：

1. probe 三个 CLI 的 executable、版本与功能；
2. 加载 user scope 和已启用项目；
3. 建立文件 watcher；
4. 做一次完整 bounded scan；
5. 恢复未完成 projection jobs；
6. 持续对账，并用低频 rescan 弥补 watcher 丢事件。

GUI 只订阅状态和发起用户动作，不成为第二 watcher 或 writer。

### 10.2 变更去环

每次成功投影记录 expected base hash、rendered hash 与 write token。

- watcher 观察到 rendered hash：视为自己的写入，no-op；
- hash 不同：作为外部编辑进入 Reconciler；
- projection 前再次比较 expected external hash，失败则放弃本次写入并重新 reconcile；
- 同一资产的 scan/reconcile/project job 串行，不同资产可有限并行；
- 连续文件事件合并防抖，目录级 rename 触发一次 manifest rescan。

### 10.3 文件系统与数据库提交边界

数据库与文件系统无法组成真正单事务，因此使用持久 job：

```text
Hub revision commit
  → projection job: prepared
  → 写临时文件/目录
  → precondition recheck
  → 原子替换
  → 校验目标 hash
  → materialization committed
```

crash 后根据 job、临时文件、备份和实际目标 hash 判定继续、回滚或重新 reconcile。不得仅因 DB 已提交就报告文件已同步。

文件写入使用现有原子保存模式。完整 Skill/Plugin 目录使用 sibling staging + backup + rename；目标目录存在未知外部文件时不做整目录删除，而进入 drift/preview。

## 11. 项目启用与跨设备项目映射

### 11.1 一次性项目启用

Workbench 项目默认只扫描和预览，不写入。启用页面必须列出：

- 将纳管的根/子目录；
- 将新建、修改或保持不动的文件；
- 三个 target 的 exact diff；
- 资产目录与配置键；
- 当前 worktree 是否 dirty；
- “cc-partner 不会 commit/push 项目仓库”的固定说明。

用户确认后写入 project opt-in。新增子目录 instruction 可在该项目 opt-in 下自动纳管。

### 11.2 项目身份映射

跨设备收到 project scope 时：

1. 使用已保存的 `hubProjectId → local Workbench projectId` 映射；
2. 没有映射时按规范化 Git remote fingerprint 提议候选；
3. 非 Git 或候选不唯一时保持 `unmapped`；
4. 用户确认本机项目后保存映射，并单独完成该项目的一次性写入启用。

未映射项目的 canonical 资产仍可完整进入 Hub/Git backup，但不得写入任意猜测路径。

## 12. 局域网 source push

### 12.1 产品语义

- 只由源设备用户选择目标设备并发起；
- 默认可选全 Hub、用户级、指定项目或指定资产；
- 目标收到后写入自己的 Hub，再按本机 CLI 安装情况、项目映射和 opt-in 投影；
- 不提供“在目标设备浏览源并任意 pull”的新 UI。

业务 API 没有调用者身份鉴权。`deviceId` 和 request ID 只是收敛/幂等标签，不描述为认证身份。

### 12.2 协议

单一 capability：`agent-hub.v1`。

协议分三步：

1. `prepare`：发送 snapshot manifest、revision 摘要与 object hashes，目标返回缺失 object；
2. `objects`：分块发送缺失 blob，逐个校验 SHA-256；
3. `commit`：在目标数据库事务中导入 revisions/variants，并登记幂等 outcome。

同一 `sourceDeviceId + clientRequestId`：

- manifest hash 相同：返回原 outcome；
- hash 不同：返回 conflict；
- 传输中断：不提交半个 snapshot，下次可复用已校验 object。

传输保留现有 Host/Origin/Content-Type、body 上限、路径校验与资源上限。这些是协议完整性约束，不是设备身份鉴权。

MCP headers、env、URL credentials 和其他凭据不脱敏、不删除、不替换为 placeholder。

## 13. Git 自动 push 与确认导入

### 13.1 自动 push

- 复用 Settings 中的私有仓库、分支和本机 Git 凭证；
- snapshot 改变后防抖并进入现有 cloud singleflight；
- 只更新本 device lane；
- push 前允许 fetch/rebase 远端 Git 历史，但不得因此导入其他 lane；
- non-fast-forward 时有界重试，仍失败则进入 Attention；
- snapshot 内容 hash 不变时不创建空 commit。

### 13.2 pull/import

“检查云端版本”只读取 snapshot manifest 并生成 preview：

- 远端 device/revision；
- user/project scope；
- 新增、修改、删除与冲突数量；
- 明文 credential-bearing assets 标记；
- project mapping 状态。

只有用户确认后才把选择的远端 revisions 导入 Hub。导入后仍走统一 Reconciler 与 ProjectionScheduler。

## 14. Agent CLI 运行时支持

资产同步和 Agent 运行时保持两个接口，避免文件格式变化影响 Orchestrator。

### 14.1 Runtime registry

统一 `AgentRuntimeAdapter` 能力：

- `probe`
- `build_launch_plan`
- `build_resume_plan`
- `completion_strategy`
- `runtime_association`
- `activation_hint`

注册：

- `claudeCodeVisible`
- `codexVisible`
- `openCodeVisible`

现有 Codex adapter 迁入统一 registry，不另写第二套 dispatcher。新增 OpenCode adapter 在 Workbench 当前 worktree 的可见 tmux terminal 启动。

### 14.2 completion 与 session

- 优先使用 adapter 已验证的结构化 hook/runtime 事件；
- 无稳定接口时使用 Orchestrator 标准独立哨兵行；
- 不从任意普通 stdout 文本猜测完成；
- native session ID 只在可靠时关联，未知不阻塞 Runner；
- 资产更新不修改运行中的 session；UI 显示下次 session/restart 生效。

### 14.3 首版范围

Workbench 新建 Agent terminal 与 Orchestrator provider picker支持三种 CLI。Prompt 优化、Git commit message、GitHub Trending 解说等现有 Claude headless 调用本轮保持 Claude-only；它们以后可基于单独的 `HeadlessAgentAdapter` 扩展，避免把 provider/model/auth 配置混入本 spec。

## 15. 用户表面

现有“Claude Code 资产”升级为“Agent CLI 资产”，复用现有 AssetRow、搜索、类型筛选和状态反馈模式：

- CLI 状态：安装路径、版本、adapter 支持级别、上次扫描；
- scope：用户级 / 项目 / 子目录；
- asset kind：Instructions / Skills / Commands / Agents / MCP / Plugins；
- target matrix：Claude / Codex / OpenCode 的 synced、partial、conflict、unsupported、activationRequired；
- shared/adapted/targetOnly 块编辑；
- project enable preview；
- LAN 选择目标并 push；
- Git snapshot 与确认导入；
- conflict diff 与解决入口。

Attention 新增 `agentHubConflict` 和 `agentHubProjectionBlocked` source，只导航到统一资产页，不在 Attention 内执行覆盖动作。

旧 `/claude-md` 与 `/claude-code` 路由保留重定向；旧页面不再拥有独立 watcher 或同步状态。

## 16. 错误处理与恢复

- CLI 未安装：保留 canonical，target 显示 unsupported，不删除该 target 的历史 variant。
- CLI 版本未知：只执行 adapter 声明为兼容的读操作，写入 blocked。
- 配置解析失败：保留原文件，生成 projection blocked；禁止用空配置覆盖。
- precondition hash 变化：重新 reconcile，不重试盲写。
- 同块、同文件、同 MCP key 或 delete-vs-edit 冲突：建立 Conflict。
- 可移植 component 成功、runtime component 不可移植：Plugin partial。
- object hash、snapshot schema 或路径非法：整个 import/commit 失败，不产生 active revision。
- projection crash：job ledger 对账并恢复，不把 prepared 当 committed。
- Git push 失败：保留本地 snapshot 和待推 job，不影响同机投影。
- LAN push 部分目标失败：按目标分别报告；成功目标不回滚。

用户解决 conflict 时可选择 Hub、外部目标、其他 target variant 或手工合并。解决结果总是生成新 revision，不改写历史 revision。

日志继续遵守项目现有诊断脱敏合同，避免在 `backend.log` 回显正文或凭据；这不影响 Hub、LAN payload、Git snapshot 与目标文件保存完整内容。

## 17. 迁移与兼容

### 17.1 本机迁移

首次启用 Agent Hub：

1. 使用现有 CLAUDE.md 文件/SQLite 对账逻辑确定当前 user 文本；
2. 迁移为 user instruction asset 和首个 revision；
3. 扫描 Claude skills、commands、plugins、MCP；
4. 检测 Codex/OpenCode 原生资产；
5. 生成只读 preview；
6. 用户级确认后启用自动投影；项目逐个确认。

现有 `claude_md` 数据保留。一个回滚窗口内把 user Claude 投影摘要 dual-write 到旧表；旧表不再参与冲突裁决。

### 17.2 P2P 与旧版本

- 新客户端只用 `agent-hub.v1` 做新 Hub push；
- 旧 Claude assets inventory/bundle 与 CLAUDE.md push 路由在迁移窗口内保持原行为，但不接收/产生多 CLI Hub revision；
- 不用旧路由伪造新 Hub 同步成功；
- 移除旧路由前先更新 P2P inventory、协议文档和混合版本测试。

### 17.3 回滚

- 关闭 Agent Hub watcher/projector 后，三个 CLI 保留最后一次成功物化文件；
- 新表和 object 不自动删除；
- 未完成 job 在旧版本中保持不可见；
- 回滚版本继续使用旧 CLAUDE.md/Claude assets 功能，不得把新表当空数据清理。

## 18. 测试与验收

### 18.1 单元测试

1. Scope/project/directory 规范化和跨设备 mapping。
2. 三个 CLI 的用户级、根目录和嵌套指令路径。
3. 单来源首次导入整篇 shared。
4. 多来源 identical/shared 与 unique/targetOnly 划分。
5. shared、adapted、targetOnly 的正向渲染和反向编辑映射。
6. 用户示例的 `instruction.discovery.before-edit` 三目标确定性输出。
7. Skill frontmatter、supporting tree、alias 与 target extension。
8. Command→Codex Skill、Agent→Codex config、MCP JSON/TOML/JSONC round-trip。
9. Plugin component decomposition 与 partial/source-only 状态。
10. 文本、目录树、MCP key、delete-vs-edit 三方合并。
11. write token 去环、precondition race、job crash recovery。
12. snapshot 确定性与 object GC 引用关系。

### 18.2 集成测试

使用隔离临时 HOME 和三个 fake CLI fixture：

- 修改嵌套 `CLAUDE.md` 的 shared 正文后，同目录两个 AGENTS 投影更新且上级内容不被复制；
- 修改 Codex adapted variant 不覆盖 Claude/OpenCode variant；
- 同时修改两个 target 的不同 shared 块自动合并；
- 同块冲突进入 Attention，相关 asset 停止投影；
- sidecar 运行、GUI 关闭时 watcher 仍收敛；
- CLI 后安装时 pending target 自动 materialize；
- project 未 opt-in 时零写入，启用后写入但 Git index/HEAD 不变化；
- MCP secret fixture 在本机投影、LAN 目标和 Git snapshot 字节一致，日志不含 fixture；
- LAN prepare/object/commit 可断点、幂等且不半提交；
- Git 自动 push 只改本 device lane，远端 lane 不自动导入；
- OpenCode visible Runner 可启动、完成、失败和保留接管现场。

### 18.3 真实 CLI 合同测试

对支持矩阵中的最低版本和当前版本分别验证：

- 指令发现顺序与嵌套覆盖；
- Skill discovery；
- MCP 启用；
- Agent/Command 映射；
- Plugin package/activation 或 activationRequired；
- 更新后是即时、新 session 还是重启生效；
- Workbench/Orchestrator launch 与 completion。

未执行的真实 Windows/Linux/macOS 场景必须保持 `NOT VERIFIED`，不能用单元测试替代 L3 结论。

### 18.4 完成标准

1. 同机三个 CLI 对 user 和 opted-in project 的共享资产自动收敛。
2. 子目录普通 `CLAUDE.md` 变更原样同步到同目录 Codex/OpenCode 文件。
3. CLI 专属规则不泄漏到其他目标。
4. 任一并发覆盖都能自动三方合并或形成可见 conflict，不静默丢失。
5. Plugin UI 准确区分 full、partial、source-only、activationRequired。
6. LAN 只提供源侧手动 push，完整传输 credential-bearing assets。
7. Git 自动 push 完整明文 snapshot，任何 pull/import 都经过确认。
8. 项目文件可后台更新，但 cc-partner 从不自动 commit/push 项目仓库。
9. OpenCode 可作为可见 Workbench/Orchestrator provider。
10. 既有 Claude 数据可迁移且具备数据库/文件回滚路径。

## 19. 实现时需要更新的持久文档

- `docs/prd.md`：产品从 Claude-only 资产升级为 Agent Hub；修改 user CLAUDE.md、LAN、Git、资产管理和 Settings 行为，明确完整凭据同步。
- `docs/p2p-protocol.md`：`agent-hub.v1` capability、prepare/objects/commit、幂等、资源上限与旧路由迁移。
- `docs/development/quality-matrix.json` 与 `docs/development/testing.md`：增加真实 CLI L2/L3 evidence。
- `src-tauri/CLAUDE.md`：Agent Hub 模块、sidecar owner、schema、adapter、projection job 与 runtime registry。
- `web/CLAUDE.md`：统一资产页 controller/view、Attention source、project preview 与测试入口。
- 根 `AGENTS.md`：若组件清单或顶级目录职责发生变化，再做精简更新。

## 20. 官方行为依据

- Codex 分层指令与 override 优先级：<https://learn.chatgpt.com/docs/agent-configuration/agents-md>
- Codex Skills 与用户/项目路径：<https://learn.chatgpt.com/docs/build-skills>
- Codex Plugin、personal/repo marketplace 与 manifest：<https://developers.openai.com/plugins/build/plugins>
- Codex App Server 外部 Agent 配置导入：<https://learn.chatgpt.com/docs/app-server>
- OpenCode Rules：<https://opencode.ai/docs/rules/>
- OpenCode Skills：<https://opencode.ai/docs/skills>
- OpenCode Commands：<https://opencode.ai/docs/commands/>
- OpenCode Agents：<https://opencode.ai/docs/agents>
- OpenCode MCP：<https://opencode.ai/docs/mcp-servers/>
- OpenCode Plugins：<https://opencode.ai/docs/plugins/>

目标 adapter 的行为由 CLI 版本 probe 和合同测试约束。官方文档变化时升级 adapter schema，不直接改写 Canonical Revision。
