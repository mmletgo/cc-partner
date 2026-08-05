# Agent Hub 用户级指令管理 V2 设计

- 日期：2026-08-04
- 状态：部分实施（scan-only 基线已交付；真实写入/删除 L3 仍为 NOT VERIFIED）
- 目标 CLI：Claude Code、Codex CLI、OpenCode
- 适用范围：Agent Hub 中当前 OS 用户级指令的发现、纳管、编辑、投影、冲突、删除、恢复与同步反馈
- 不适用范围：项目级指令、Skill / Command / Agent / MCP / Plugin 的通用资产管理；这些能力继续遵循原 Agent Hub 合同
- 上位文档：[`2026-07-29-multi-cli-agent-hub-design.md`](./2026-07-29-multi-cli-agent-hub-design.md)

## 0. 文档地位与替代关系

本 Spec 是“多 CLI Agent Hub”在**用户级指令文件**这一垂直领域的 V2 产品与技术合同。它不推翻 Canonical Hub、Revision DAG、CAS、Snapshot、LAN source-push、Git device-lane 等基础架构，但替代旧合同中以下面向用户级 instruction 的具体约定：

1. 首屏继续复用通用 `AgentAssetRow + target matrix` 的交互方式；
2. 以 `desiredPresence × desiredEnabled` 直接驱动用户级指令 UI；
3. 将用户未选择的 target 计入 `partial`；
4. 固定把 Codex 用户级 Hub 投影写入 `AGENTS.override.md`；
5. 仅从已有 materialization 推断用户级原生文件；
6. 用“全部删除”同时表达停止管理、本机投影删除和 canonical tombstone；
7. 在 mutation 之后再展示所谓“预览”。

旧文档中与本 Spec 冲突的用户级 instruction 条款，以本 Spec 为准。portable asset、项目 opt-in、Snapshot 和 Runtime 合同不受影响。

完整实施完成前，以已落地的 scan-only 页面和 fail-closed 后端为生产事实；不得仅凭本 Spec 或 preview/apply 命令存在就宣称真实 CLI 写入已交付。

## 1. 摘要

Agent Hub 的用户级指令入口必须从“内部资产状态矩阵”改为“用户级指令管理器”。用户进入页面后，系统必须优先回答：

1. 当前三个 Agent 实际读取哪些文件；
2. 哪些文件由 Hub 管理，哪些只是外部原生文件或兼容回退；
3. 用户要把哪些内容应用到哪些 Agent；
4. 下一次确认会创建、修改、删除或遮蔽哪些文件；
5. 出现外部修改、删除或冲突后如何恢复且不丢内容。

完整主链为：

```text
只读发现
  → 展示实际生效来源与优先级
  → 用户选择公共内容、专属内容和目标 Agent
  → 生成逐目标路径、优先级影响与 before/after diff
  → 用户确认
  → 原子提交 Hub 管理意图
  → 每个目标独立执行安全投影
  → 展示逐目标成功、阻断、冲突或重试结果
```

确认前不得写入任何 CLI 原生文件，不得创建会改变 CLI 发现顺序的空文件，不得把扫描到的当前 hash 自动解释成 Hub 已拥有该文件。

## 2. 问题与当前基线

### 2.1 首次纳管链路断裂

当前 legacy migration 会创建一个 `User CLAUDE.md` canonical asset，并为 Claude、Codex、OpenCode 创建 `desiredPresence=absent`、`desiredEnabled=false` 的 binding。前端只提供 enable toggle；设置 target presence 为 `present` 的能力没有进入正常用户旅程。

因此页面可以展示“应不存在”，却没有“添加到 Claude / Codex / OpenCode”的可执行主操作。用户无法完成旧合同所说的“确认后才 present”。

### 2.2 状态语义互相矛盾

当前 UI 可同时展示：

- `应不存在`
- `无状态`
- `已验证`
- `不支持`
- aggregate `部分`

这些值来自多个可任意组合的内部字段，而不是用户状态机。已知错误包括：

1. capability 为 `readOnly` 时仍可能被归类为 unsupported；
2. `desiredPresence=absent` 且没有 materialization 时可能被标记 `verified=true`；
3. 从未被用户选择的 target 仍会参与 aggregate 计算；
4. 顶部 `writeCompatible` 只代表 Hub 前后端协议兼容，不代表 CLI adapter 可以安全写入。

### 2.3 发现不完整

当前生产 scanner 主要扫描已有 materialization。没有 materialization 的 Codex/OpenCode 用户级文件可能完全不进入 Hub inventory。结果是：

- Hub 不知道用户真正正在使用哪个文件；
- Hub 无法说明 active、fallback、shadowed 关系；
- 首次接管前无法给出可信 diff；
- 只有 Claude legacy migration 能稳定生成用户级 canonical 入口。

### 2.4 写入与删除边界不安全

当前模型一旦开放写能力，存在以下风险：

- 创建 Codex `AGENTS.override.md` 后遮蔽现有 `AGENTS.md`；
- 直接覆盖 OpenCode 已有 `AGENTS.md`；
- `targetOnly` 内容投影到非来源 Agent 时生成空文件；
- 把第一次扫描到的外部 hash 当成可覆盖 base；
- 把 observed external hash 当成可安全删除的 ownership 证据；
- projection enqueue 失败时 mutation 仍以成功返回；
- “全部删除”同时删除 canonical 和多个目标文件，却没有路径级预检与完整恢复入口。

### 2.5 当前真实能力必须诚实呈现

截至本 Spec 编写时，support manifest 中三个 Agent 的 instruction scan 都只具备只读能力，render instruction 被阻断，真实 CLI 版本上下界尚未完成认证。

因此 V2 可以先交付可信 inventory、来源解释、比较和草稿，但在相应 target 的 L3 写盘证据完成前，必须继续 fail closed：

- 可以读取、比较和打开文件；
- 不得展示可执行的“自动应用”主按钮；
- 不得把 Hub API 兼容表述成“允许写入 Agent”；
- 不得把 L1/L2 adapter 测试升格成真实 CLI 写盘认证。

## 3. 官方事实基线

路径与优先级属于外部产品合同，可能随 CLI 更新而变化。Adapter 和 support manifest 是运行时权威，以下事实是本 Spec 的设计基线，不允许被前端硬编码为永远不变的常量。

### 3.1 Claude Code

- 标准用户级指令文件：`~/.claude/CLAUDE.md`；
- 实际配置根需继续支持项目已有的 `CLAUDE_CONFIG_DIR` 解析；
- 用户文件会在 Claude Code 启动时进入上下文。

官方参考：[Claude Code 管理内存](https://docs.anthropic.com/zh-CN/docs/claude-code/memory)

### 3.2 Codex

- Codex home 默认是 `~/.codex`，可由 `CODEX_HOME` 改写；
- 全局级先检查 `AGENTS.override.md`，存在时不会再使用同层 `AGENTS.md`；
- 官方创建持久全局规则的主路径是 `~/.codex/AGENTS.md`；
- `AGENTS.override.md` 用于临时全局 override，移除后恢复基础规则。

官方参考：[Codex AGENTS.md 指南](https://learn.chatgpt.com/docs/agent-configuration/agents-md)

由此确定：

1. Hub 不得默认永久创建 `AGENTS.override.md`；
2. 默认新建投影路径为 adapter 当时解析出的 Codex `AGENTS.md`；
3. 如果 override 已存在，inventory 必须同时返回 override 和被遮蔽的 base；
4. 只有用户显式选择管理 override，或 adoption 的 active source 本身就是 override，Hub 才能投影到 override；
5. 创建或更新 override 的 preview 必须显示 `willShadowSourcePath`。

### 3.3 OpenCode

- 标准全局规则文件：`~/.config/opencode/AGENTS.md`；
- 配置根继续支持项目已有的 `OPENCODE_CONFIG_DIR` / `XDG_CONFIG_HOME` 解析；
- 如果原生全局 `AGENTS.md` 不存在，OpenCode 可回退到 `~/.claude/CLAUDE.md`；
- `OPENCODE_DISABLE_CLAUDE_CODE` 或 `OPENCODE_DISABLE_CLAUDE_CODE_PROMPT` 可以禁用该回退；
- 原生 OpenCode `AGENTS.md` 优先于 Claude fallback。

官方参考：[OpenCode Rules](https://opencode.ai/docs/rules/)

由此确定：

1. “OpenCode 原生文件不存在”不等于“OpenCode 没有用户级指令”；
2. inventory 必须识别兼容回退是否开启；
3. UI 必须能显示“当前继承 Claude 用户级指令”；
4. 新建 OpenCode 原生文件会改变实际生效来源，preview 必须显示 `willReplaceFallbackSourcePath`；
5. Hub 不得在未识别禁用环境变量时声称 fallback 正在生效。

### 3.4 事实更新规则

任何修改 adapter 路径、优先级、版本支持、激活方式或环境变量规则的实现，必须：

1. 核对对应 CLI 官方文档；
2. 更新 support manifest 与 evidence；
3. 更新本 Spec 或最相关的开发文档；
4. 增加隔离 HOME 测试；
5. 在真实 CLI 版本未验证前保持写入 fail closed。

## 4. 目标与非目标

### 4.1 目标

1. 让非开发者理解自己的用户级指令当前从哪里生效。
2. 让用户在一次安全流程中选择要由 Hub 管理的内容和 Agent。
3. 在任何目标写入前展示真实路径、操作类型、优先级影响和精确 diff。
4. 把公共规则与 Agent 专属规则统一纳入一个可理解的编辑模型。
5. 明确区分未管理、已管理、兼容继承、外部修改、外部删除、路径冲突和能力阻断。
6. 保证现有外部文件在未确认 adoption 前不被覆盖、删除或遮蔽。
7. 支持停止管理、单目标移除、canonical 删除和版本恢复等不同意图。
8. 让 LAN/Git 的“内容已导入”与“已应用到本机 Agent”成为两个不同结果。
9. 保留高级 ledger、revision、scope 和 adapter 诊断，但不让它们占据首次使用主界面。

### 4.2 非目标

- 不把三个 Agent 的整个配置目录纳入 Hub。
- 不同步账号、登录、API key、OAuth token、session 或模型选择。
- 不在后台使用大模型自由翻译或改写用户指令。
- 不保证已启动的 CLI session 热加载新文件。
- 不在没有真实版本 evidence 时开放自动写入。
- 不把不同 CLI 的项目级文件路径问题混入本次用户级改造。
- 不重做 portable asset 的通用 target matrix；其高级视图继续存在。
- 不新增 LAN 身份认证、可信设备或权限 capability token。

## 5. 产品原则

### 5.1 先解释生效事实，再要求用户做决定

用户不应先看到“应存在/不存在”，而应先看到“Claude 当前读取这个文件”“OpenCode 正在继承 Claude”“Codex 的 base 文件被 override 遮蔽”。

### 5.2 发现不等于纳管

扫描到文件只建立 inventory 事实，不自动建立 ownership，不自动创建 target binding，不自动修改 canonical head，不自动写盘。

legacy migration 已产生的 canonical 可以作为“待确认草稿”展示，但不得因为它存在就声称目标已由 Hub 管理。

### 5.3 未选择不是失败

用户没有选择某个 Agent 时，该 target 是中性的 `unmanaged`，不参与同步健康度，不产生 `partial`、blocked 或 error。

### 5.4 预览必须发生在 mutation 之前

任何可能改变 canonical、binding、ownership 或目标文件的操作都必须先 preview。preview 不得先调用 mutation 再展示新状态。

### 5.5 文件 ownership 必须显式

只有以下条件全部满足时，Hub 才能覆盖或删除目标文件：

- 用户完成 adoption 或 Hub 自己创建该文件；
- ownership 记录仍有效；
- 当前文件 hash 与 preview 的 expected hash 一致；
- 当前操作由未过期 plan token 授权。

### 5.6 指令文件不伪装成有原生启停开关

用户级 Markdown 指令通常没有稳定的 CLI 原生 enabled 状态。V2 不直接展示 `desiredEnabled` toggle。

“暂停在此 Agent 使用”必须被实现为：在明确提示后安全移除 Hub-owned 投影，同时保留 canonical 和 target 选择；如果 hash 或 ownership 不满足安全删除条件，则进入 drift/collision 解决流程，不能声称已暂停。

### 5.7 所有危险动作可解释、可恢复

停止管理、暂停使用、从单个 Agent 移除、删除 canonical 是四个独立意图。任何删除都要列出路径、影响和恢复点。

## 6. 信息架构

### 6.1 Agent Hub 顶层

Agent Hub 使用以下一级分区：

1. **用户级指令**：默认入口；本 Spec 的主要界面。
2. **项目级指令**：选择 Workbench 项目后管理；继续遵循项目 opt-in 合同。
3. **Skills 与工具**：Skill / Command / Agent / MCP / Plugin 等 portable 资产。
4. **同步与导入**：LAN Push、Git snapshot inspect/import、同步历史。
5. **兼容性与诊断**：CLI probe、版本、adapter evidence、scope/revision ledger、阻断详情。

在窄窗口下使用现有可访问 tab/导航模式；不得把五个一级分区压成不可读的状态 chip。

### 6.2 页面标题区

默认标题：`用户级指令`

副标题：`管理会应用到所有项目的个人规则，并控制 Claude、Codex 与 OpenCode 各自使用的文件。`

标题区仅保留：

- 刷新扫描；
- 页面级主 CTA；
- 更多菜单中的“打开兼容性与诊断”。

`预览项目启用`、`LAN Push`、`Git 导入` 移出首屏标题区。

### 6.3 状态摘要

摘要必须用一句人类可读的句子表达当前事实，例如：

- `已发现 1 份用户级指令，Agent Hub 尚未管理任何文件。`
- `2 个 Agent 已由 Hub 管理，OpenCode 正在继承 Claude。`
- `Claude 和 Codex 已同步，OpenCode 有 1 项需要处理。`

摘要下可展示最近更新时间，但不得展示装饰性统计数字。

### 6.4 用户级指令编辑区

编辑模型使用四个内容面：

- 公共规则；
- Claude 专属补充；
- Codex 专属补充；
- OpenCode 专属补充。

默认打开公共规则。只有确实存在专属内容或用户主动选择时才强调专属 tab。

现有 Instruction Block 编辑能力进入“高级编辑”Drawer。普通编辑器展示完整 Markdown 草稿，并提供：

- 未保存标记；
- 字符数；
- 放弃草稿；
- 预览并应用；
- 文件变化导致预览失效后的草稿保留。

### 6.5 Agent 目标卡片

Claude、Codex、OpenCode 各一张 target 卡片，信息顺序固定为：

1. Agent 名称和 CLI 安装/版本状态；
2. 用户态主状态；
3. 当前实际生效来源和路径；
4. Hub 管理路径；
5. 能力解释；
6. 最近同步/外部变化；
7. 当前可执行动作。

默认不展示 canonical key、scope id、origin namespace、规范名或调用别名。这些字段只进入诊断抽屉。

## 7. 核心用户旅程

### 7.1 首次进入：没有任何文件

页面显示：

> 尚未发现用户级指令。你可以在 Agent Hub 创建一份公共规则，再选择要应用的 Agent。

主 CTA：`创建用户级指令`

用户可以先编辑草稿；在点击“预览并应用”前，不创建任何 CLI 文件。

### 7.2 首次进入：发现单一来源

示例：仅发现 Claude `~/.claude/CLAUDE.md`。

页面显示：

> 已发现 Claude 用户级指令。当前只有 Claude 使用这份内容，Agent Hub 尚未管理它。

主 CTA：`开始管理`

向导提供：

1. `只继续用于 Claude`：导入为 Claude 专属内容；
2. `作为公共规则`：将内容提升为 common，并选择其他目标；
3. `稍后处理`：保持完全不受管。

默认选择第一项，不默认跨 Agent 扩散。

### 7.3 首次进入：发现多个完全相同来源

按规范化 block/hash 判断内容完全相同时，建议合并为公共规则，但仍需用户确认 ownership 和目标路径。

页面必须说明：合并只改变 Hub 中的表达方式，不会在确认前修改任何原文件。

### 7.4 首次进入：发现多个不同来源

默认保留各 target 专属内容。系统可以确定性提取逐字相同的完整 block 作为“建议公共部分”，但不得使用后台 LLM 猜测语义等价。

用户可逐块选择：

- 设为公共；
- 保留为 Agent 专属；
- 删除草稿中的重复内容。

任何原文件保持不变，直到统一 preview/confirm。

### 7.5 选择目标 Agent

每个 target 显示以下选择之一：

- `由 Hub 管理`
- `保持现状，不管理`
- `仅继承现有来源`，只适用于 adapter 证明存在 fallback 的场景

能力为 scan-only/blocked 时，“由 Hub 管理”不可选，并显示具体原因和“打开文件”动作。

### 7.6 预览并应用

确认页按 target 分组，逐一显示：

- 当前生效文件；
- Hub 将管理的文件；
- operation：`create | update | delete | leave`；
- before/after diff；
- 预期 hash；
- 是否会遮蔽或替换 fallback；
- 生效方式：即时、新 session、重启或未知；
- capability 阻断与 evidence 状态。

主确认按钮文案包含目标数量，例如：`应用到 2 个 Agent`。

如果没有可执行 change，按钮禁用并说明原因。

### 7.7 应用结果

数据库中的 canonical revision、目标选择和 ownership adoption 作为一个事务提交。文件系统投影按 target 分别原子执行，因此结果允许部分成功，但必须逐目标返回：

- 已应用；
- 无需更改；
- 文件已变化，需要重新预览；
- 当前版本仅支持扫描；
- 写入失败，可重试；
- 激活/重启要求。

projection enqueue 或写入失败不得返回无条件成功。已成功 target 不强制回滚；失败 target 保留可重试 plan/context，不得伪装成 full。

### 7.8 日常编辑

编辑始终先进入本地草稿。点击“预览并应用”后，系统计算所有 managed active target 的新渲染和 diff。

用户取消 preview 时草稿继续保留。应用失败、preview 过期或目标文件变化时草稿也必须保留。

### 7.9 外部修改：Drift

检测到 Hub-owned 文件 hash 变化时，不自动覆盖。卡片显示：

> 文件在 Hub 外被修改。

提供：

- 比较修改；
- 接受外部版本；
- 用 Hub 版本恢复；
- 停止管理并保留文件。

非重叠 block 可以生成自动合并预览；同一 block 冲突必须进入人工解决。

### 7.10 外部删除：Detached

检测到受管文件消失时，不自动重建。提供：

- 恢复文件；
- 接受删除并暂停此 target；
- 停止管理此 target。

### 7.11 路径已有外部内容：Collision

首次纳管或切换目标路径时发现未 owned 内容，必须保留文件并提供：

- 纳管现有文件；
- 比较后用 Hub 版本替换；
- 保持不受管。

所有确认都带 expected hash。文件变化后旧确认立即失效。

### 7.12 暂停、停止管理、单目标移除与全部删除

四个动作必须分开：

#### 暂停在此 Agent 使用

- canonical 和 target 选择保留；
- 仅安全删除 Hub-owned 投影；
- 后续可一键 preview/resume；
- 如果当前文件已 drift，不执行删除。

#### 停止由 Hub 管理，保留文件

- 解除 target ownership 和 binding；
- 本地字节保持不变；
- 后续外部编辑不再进入 Hub drift；
- target 回到 `unmanaged`。

#### 从此 Agent 移除指令

- 删除 target 选择；
- 仅在 ownership 和 hash 精确匹配时删除 Hub-owned 文件；
- canonical 与其他 target 不受影响；
- 必须先 preview 具体路径。

#### 删除用户级指令及所有受管文件

- 只出现在危险区；
- preview 列出所有 target、路径、共享引用、无法删除项和恢复 revision；
- 需要输入资产显示名确认；
- canonical 追加 tombstone revision；
- 每个文件仍按 ownership/hash 安全删除；
- 结果按 target 展示；
- 提供 `restoreDeletedAsset` 以新 revision 恢复。

## 8. 内容模型

### 8.1 Canonical 文档

用户级指令继续使用：

- common payload；
- Claude target extension；
- Codex target extension；
- OpenCode target extension。

UI 将其表达为“公共规则 + Agent 专属补充”，不直接暴露 `policy` 和 block map。

### 8.2 单一来源 adoption

单一来源默认导入为来源 target 的专属内容。只有用户明确选择“作为公共规则”，才把内容提升为 common。

这条规则避免把 Claude 特有工具名、slash command、路径或运行时约定静默投影到 Codex/OpenCode。

### 8.3 跨 target 空输出保护

compiler 输出必须同时报告：

- `renderedBytes`；
- `includedBlockIds`；
- `excludedTargetOnlyBlockIds`；
- `emptyDueToTargetOnly`。

如果目标输出为空且原因是全部内容都属于其他 target，preview 必须阻断 create/update，不能生成零字节文件。

### 8.4 原生文件反向编辑

受管文件发生外部修改时，仍使用 materialization base、Hub current、external current 做三方合并。

外部文件中的未知内容必须保留为 target extension 或 conflict，不允许在 normalize 时静默丢弃。

## 9. 用户态状态模型

V2 将用户状态拆成五个正交维度，后端 DTO 必须保证组合合法；前端不得自行拼接多个任意布尔 badge。

### 9.1 CLI 状态

```text
notInstalled | installed | versionUnknown
```

### 9.2 来源解析状态

```text
missing | native | override | fallback | shadowed
```

一个 target 可以返回多个 source，但恰好零个或一个 `active=true`。如果 adapter 无法证明 active source，返回 reason code，不猜测。

### 9.3 管理状态

```text
unmanaged | managedActive | managedPaused
```

- `unmanaged`：Hub 不拥有目标文件；
- `managedActive`：Hub 拥有并期望目标投影生效；
- `managedPaused`：Hub 保留 canonical/target 意图，但当前没有 active Hub-owned 投影。

instruction UI 不暴露 `desiredEnabled`。旧字段可在数据库兼容期保留，但 V2 service 必须原子地归一化为 management mode。

### 9.4 能力状态

按动作分别返回，不再压成一个 `supported` 布尔值：

```text
scan: supported | readOnly | blocked
write: supported | blocked
remove: supported | blocked
activate: immediate | newSession | restart | unknown | blocked
```

每项包含稳定 `reasonCode`、用户可读 message key、adapter schema version、CLI version 和 evidence IDs。

### 9.5 投影状态

```text
none
pending
inSync
drift
detached
conflict
collision
activationRequired
failed
blocked
```

`verified` 不再作为独立 UI badge。只有以下事实可以生成验证文案：

- 当前文件 hash 与 Hub rendered hash 一致：`文件已是最新`；
- 真实 CLI L3 验证通过：`已在此 CLI 版本验证`，并显示 evidence；
- 路径不存在只表示 `未发现文件`，不能显示“已验证”。

### 9.6 页面聚合状态

设置阶段：

```text
unconfigured | readyToReview | configured
```

健康阶段：

```text
healthy | actionRequired | blocked
```

规则：

1. 没有 `managedActive` 或 `managedPaused` target 时为 `unconfigured`；
2. 只有显式 managed active target 参与同步健康计算；
3. paused 和 unmanaged 为中性，不产生 partial；
4. 所有 active target `inSync` 时显示“所选 Agent 均已同步”；
5. 其余显示 `N 项待处理`，不得只显示抽象 `partial`。

### 9.7 文案映射

| 内部/旧文案 | V2 默认文案 |
|---|---|
| `desiredPresence=absent` 且无用户选择 | 未纳入管理 |
| `scanOnly` / `readOnly` | 可读取，暂不自动写入 |
| `synced + hash match` | 文件已是最新 |
| `targetOnly` | 仅此 Agent |
| `adapted` | 各 Agent 有专属版本 |
| `partial` | N 项待处理 |
| `detached` | 文件在外部被删除 |
| `externalCollision` | 已有同名内容，需要先比较 |
| `writeCompatible` | 后端连接正常 |
| `sourceOnly` | 只保存在 Hub，没有对应 CLI 文件 |

## 10. 后端领域合同

### 10.1 User Instruction Inventory

新增独立于 materialization 的用户级 inventory。每次显式刷新和受控后台刷新应：

1. 解析三个 target 的 config home；
2. 枚举 adapter 声明的全部候选用户级来源；
3. 读取文件 metadata、hash 和受限大小的内容；
4. 计算优先级、active、fallback 和 shadowed；
5. 识别 ownership；
6. 识别兼容回退开关；
7. 返回事实，不创建 binding、不写文件。

指令内容可能包含私有信息。日志、diagnostic summary、Attention 和错误信封不得输出正文，只允许 path、hash、大小、状态、reason code 和脱敏摘要。

### 10.2 V2 Overview DTO

```ts
interface UserInstructionWorkspaceDto {
  scopeId: string
  inventorySnapshotHash: string
  setupState: 'unconfigured' | 'readyToReview' | 'configured'
  healthState: 'healthy' | 'actionRequired' | 'blocked'
  canonical: {
    assetId: string
    displayName: string
    headRevisionId: string | null
    commonContent: string
    targetExtensions: Partial<Record<AgentTarget, string>>
    deleted: boolean
  } | null
  targets: UserInstructionTargetDto[]
  refreshedAt: string
}

interface UserInstructionTargetDto {
  target: 'claude' | 'codex' | 'opencode'
  cli: {
    installed: boolean
    version: string | null
    configRoot: string
  }
  sources: Array<{
    sourceId: string
    path: string
    role: 'native' | 'override' | 'fallback' | 'shadowed'
    active: boolean
    exists: boolean
    nonEmpty: boolean
    hash: string | null
    modifiedAt: string | null
    ownership: 'external' | 'hubManaged' | 'unknown'
  }>
  effectiveSourceId: string | null
  managedTargetPath: string
  managementMode: 'unmanaged' | 'managedActive' | 'managedPaused'
  capability: {
    scan: 'supported' | 'readOnly' | 'blocked'
    write: 'supported' | 'blocked'
    remove: 'supported' | 'blocked'
    activate: 'immediate' | 'newSession' | 'restart' | 'unknown' | 'blocked'
    reasonCode: string | null
    evidenceIds: string[]
  }
  projection: {
    state: 'none' | 'pending' | 'inSync' | 'drift' | 'detached' | 'conflict' |
      'collision' | 'activationRequired' | 'failed' | 'blocked'
    desiredRevisionId: string | null
    appliedRevisionId: string | null
    observedHash: string | null
    lastErrorCode: string | null
  }
  availableActions: UserInstructionAction[]
}
```

`availableActions` 由后端根据 capability、ownership、state 和 invariants 生成。前端不根据零散布尔自行推导危险动作。

`inventorySnapshotHash` 必须覆盖本次 inspect 使用的 target homes、候选来源路径、active/shadowed 解析、来源 hash、ownership、capability 与 canonical head。后续 preview 必须回传该值；任何相关事实变化都令旧 preview 失效。

### 10.3 Preview Plan DTO

```ts
interface UserInstructionPlanDto {
  planToken: string
  expiresAt: string
  baseRevisionId: string | null
  inventorySnapshotHash: string
  changes: Array<{
    target: AgentTarget
    path: string
    operation: 'create' | 'update' | 'delete' | 'leave'
    currentHash: string | null
    expectedHash: string | null
    renderedHash: string | null
    unifiedDiff: string | null
    ownershipRequired: boolean
    willShadowSourcePath: string | null
    willReplaceFallbackSourcePath: string | null
    emptyDueToTargetOnly: boolean
    activation: 'immediate' | 'newSession' | 'restart' | 'unknown'
    warnings: string[]
  }>
  blockingReasons: string[]
}
```

plan token 必须绑定：

- 当前 OS 用户；
- base revision；
- inventory snapshot；
- 目标选择；
- expected hashes；
- operation 集合；
- 过期时间。

### 10.4 Commands / Service API

V2 至少提供以下领域能力；具体 Tauri command 命名可遵循仓库现有命名风格，但语义不得合并：

- `inspect_user_instruction_workspace`
- `preview_user_instruction_setup`
- `apply_user_instruction_plan`
- `preview_user_instruction_update`
- `preview_adopt_user_instruction_source`
- `preview_pause_user_instruction_target`
- `preview_stop_managing_user_instruction_target`
- `preview_remove_user_instruction_target`
- `preview_delete_user_instruction_asset`
- `list_user_instruction_history`
- `restore_user_instruction_revision`
- `restore_deleted_user_instruction_asset`

旧 `setTargetPresence` / `setTargetEnabled` 可以作为内部兼容能力保留，但 V2 前端不得用两次独立 mutation 拼装一次用户动作。

### 10.5 Apply invariants

`apply_user_instruction_plan` 必须：

1. 验证 plan token、expiry 和幂等键；
2. CAS 校验 canonical base revision；
3. 重新解析 target path；
4. 校验当前 hash 与 expected hash；
5. 对 overwrite/delete 校验 ownership；
6. 阻止 `emptyDueToTargetOnly`；
7. 事务内提交 canonical、binding/management mode 和 adoption；
8. 生成持久化 projection job；
9. 返回每个 target 的真实 enqueue/执行结果；
10. 任一步的错误必须稳定、可重试、可诊断。

禁止把“调用 apply 成功”直接等同于“所有目标文件已同步”。

### 10.6 Ownership / Adoption

首次 adoption 记录至少包含：

- target；
- resolved path；
- adopted hash；
- adopted revision；
- adoption operation；
- user-confirmed plan token；
- created/confirmed timestamp。

安全删除只认可：

- Hub ownership；
- 最后一次 Hub 成功写入的 rendered hash；
- 当前磁盘 hash 精确一致。

`observedExternalHash` 不能单独作为删除许可。

### 10.7 Scheduler 结果

统一返回：

```text
queued | applied | noChange | stalePreview | blocked | conflict | failed
```

Snapshot/LAN/Git 结果必须把以下概念分开：

```text
canonicalImportedAssetIds
activationRequiredAssetIds
activationPlans
appliedTargets
blockedTargets
```

不得再用单一 `projectionsScheduled` 表述“内容已导入”和“本机已生效”。

## 11. 前端合同

### 11.1 组件与分层

V2 复用现有：

- `Button`
- `Card`
- `Dialog`
- `Drawer`
- `StatusMessage`
- `Input`

用户级指令应有专用 domain/view 组合，不继续把通用 `AgentAssetRow` 作为主界面。`AgentAssetRow` 保留给 portable assets 和高级 ledger。

建议拆分：

```text
pages/AgentHub/
  AgentHub.tsx
  useAgentHubController.ts
  userInstructions/
    useUserInstructionManager.ts
    UserInstructionView.tsx
    UserInstructionEditor.tsx
    UserInstructionTargetCard.tsx
    UserInstructionSetupDialog.tsx
    UserInstructionPreviewDialog.tsx
    UserInstructionDangerZone.tsx
```

view 不直接 import `@/api/*`；API、草稿、preview、apply 和刷新归 controller 管理。所有 hooks 必须位于 early return 前。

### 11.2 草稿保护

复用旧 ClaudeMd 页已有的未保存草稿与离开保护思路，但 source of truth 必须收敛到 Hub V2 API。

以下情况草稿必须保留：

- preview 取消；
- preview 过期；
- hash 变化；
- 单个 target 写入失败；
- Dialog 意外关闭；
- 刷新 inventory 后发现冲突。

### 11.3 状态展示

- 每张卡片只显示一个主状态和至多一个辅助能力说明；
- 不把五六个 pill 平铺；
- 颜色不能是唯一状态载体；
- 路径使用 monospace，可复制，可打开；
- blocked 必须有具体原因和下一步；
- scan-only 使用中性提示，不使用危险红色“不支持”。

### 11.4 可访问性

- 设置向导和预览使用现有 Dialog/focus trap；
- 危险删除使用 `role=alert` 的 `StatusMessage`，且一次失败只播报一次；
- target 选择使用原生 checkbox/radio 语义；
- diff 区可键盘滚动，并有纯文本摘要；
- 状态更新使用 live region，不抢夺焦点；
- 所有图标按钮提供可读 label；
- reduced-motion 下不依赖动画传达状态。

### 11.5 中文文案

中文 locale 不得出现整句英文。i18n 质量门禁除 key parity 外，应增加中文资源中的长英文句检测白名单。

## 12. 迁移与兼容

### 12.1 既有用户数据迁移

升级到 V2 时，识别旧 migration 自动生成的用户 instruction：

- display name 为 `User CLAUDE.md`；
- logical key 为 `CLAUDE.md`；
- policy 为 Claude `targetOnly`；
- 三个 binding 均为 absent/disabled；
- 没有成功 materialization 或明确用户 target 操作。

满足全部条件时：

1. 保留 canonical revision 和内容；
2. 将它标记为 `readyToReview` 的 legacy draft；
3. 删除或迁移三个“伪 requested”的 absent binding，使 target 回到真正 `unmanaged`；
4. 不写任何原生文件；
5. UI 显示“已从旧版发现 Claude 内容，等待确认管理方式”。

如果某 binding 已有成功 materialization、用户 action audit 或 ownership，则不得套用上述自动清理，必须按真实 managed 状态迁移。

### 12.2 旧页面和命令

- `/claude-md` 继续按既有 N/N+1 合同重定向；
- 旧 ClaudeMd 编辑 API 不再作为 V2 写入路径；
- legacy LAN/Git Claude 文件同步逐步收敛为 canonical facade；
- 兼容期内旧 command 只服务旧客户端，不得绕过 V2 ownership/preview 门闩。

### 12.3 回滚

数据库 migration 必须可回滚到保留旧字段的版本。回滚不得删除 canonical revision、ownership history 或外部文件。

如果 V2 UI feature flag 被关闭：

- 展示只读诊断；
- 阻断旧 UI 对 user instruction 的危险 mutation；
- portable assets 仍可继续使用旧矩阵。

## 13. LAN 与 Git 语义

### 13.1 Snapshot 内容

Snapshot 继续只携带 canonical assets、revisions、variants、conflicts 和 tombstones，不携带本机绝对路径、bindings、materializations 或 ownership。

### 13.2 导入后行为

用户级 canonical 导入本机后：

1. 状态为 `readyToReview`；
2. 不自动创建任何 target binding；
3. 不自动写 CLI 文件；
4. 必须重新运行本机 inventory；
5. 用户通过 V2 preview/confirm 选择本机目标。

### 13.3 结果文案

- `已导入到 Agent Hub`：只表示 canonical 导入成功；
- `已应用到 Claude/Codex/OpenCode`：只有目标文件投影成功才显示；
- `等待在本机设置`：canonical 已导入但尚无 target 选择。

## 14. 错误与诊断

所有错误使用稳定 code，至少覆盖：

```text
USER_INSTRUCTION_PREVIEW_STALE
USER_INSTRUCTION_REVISION_CHANGED
USER_INSTRUCTION_SOURCE_CHANGED
USER_INSTRUCTION_OWNERSHIP_REQUIRED
USER_INSTRUCTION_TARGET_SCAN_ONLY
USER_INSTRUCTION_TARGET_VERSION_UNVERIFIED
USER_INSTRUCTION_EMPTY_TARGET_RENDER
USER_INSTRUCTION_WOULD_SHADOW_SOURCE
USER_INSTRUCTION_COLLISION
USER_INSTRUCTION_DRIFT
USER_INSTRUCTION_DELETE_HASH_MISMATCH
USER_INSTRUCTION_PROJECTION_FAILED
```

用户消息解释发生了什么和下一步，不展示 tombstone、CAS、materialization 等内部术语。诊断详情可以展示 request ID、target、path、hash prefix、adapter version 和 evidence ID，但不得展示指令正文或凭据。

## 15. 验收旅程

以下旅程全部通过前，V2 不得标记完成。

### 15.1 发现与首次设置

1. Hub 未设置，仅存在 Claude 用户文件：正确显示路径、内容来源和“开始管理”；确认前磁盘零写入。
2. 没有任何文件：可创建草稿，但 preview 前不创建目标文件。
3. 只选择 Claude：页面为 healthy，不出现 partial。
4. 选择 Claude + Codex：展示两个精确路径和 diff；确认后原子提交意图，逐目标返回结果。
5. 三个来源内容不同：默认保留专属内容，不自动覆盖或语义合并。

### 15.2 Codex 优先级

6. 同时存在 `AGENTS.override.md` 与 `AGENTS.md`：正确标记 override active、AGENTS shadowed。
7. 计划创建 override：必须显示被遮蔽路径；没有显式选择时禁止。
8. 只存在 `AGENTS.md`：默认管理 base，不静默创建 override。

### 15.3 OpenCode fallback

9. 原生 OpenCode 文件缺失、Claude 文件存在、兼容未禁用：显示“继承 Claude”，不显示“无指令”。
10. `OPENCODE_DISABLE_CLAUDE_CODE_PROMPT=1`：显示 fallback 已禁用。
11. 新建原生 OpenCode 文件：preview 显示将替换 Claude fallback。

### 15.4 安全写入与冲突

12. 目标已有外部文件：进入 collision；确认前不覆盖。
13. preview 后目标文件变化：apply 返回 stale，草稿保留并要求重新预览。
14. `targetOnly` 导致空输出：阻断创建空文件。
15. scan-only 或未认证版本：只允许读取、比较、打开文件。
16. projection enqueue 失败：页面不能显示所有目标成功。
17. 非重叠外部修改：可生成自动合并 preview；同 block 修改进入 conflict。

### 15.5 删除与恢复

18. 暂停 target：仅安全移除 Hub-owned 投影，canonical 保留。
19. 停止管理并保留文件：字节不变，ownership 解除。
20. 从单个 target 移除：不 tombstone canonical，不影响其他 target。
21. 外部删除：进入 detached，不自动重建。
22. 全部删除：先列出路径和影响，生成 tombstone，逐目标返回结果，可恢复为新 revision。
23. 当前 hash 不等于最后 rendered hash：拒绝删除。

### 15.6 导入与本地激活

24. LAN/Git 导入 canonical 后不自动投影，显示“等待在本机设置”。
25. imported revision 与本地分叉时遵循现有 DAG/conflict 合同。
26. config home 变化时重新解析路径，展示 old/new 影响，不盲目移动文件。

### 15.7 可用性与无障碍

27. 复现旧截图数据时，只展示一个明确主 CTA，不出现“应不存在/无状态/已验证/不支持”矛盾组合。
28. 全键盘可完成首次设置、preview、取消和应用。
29. screen reader 能理解 target、路径、操作和逐目标结果。
30. 中文 locale 无整句英文泄漏。

## 16. 测试与证据

### 16.1 Rust 单元测试

- source precedence 与 active source 唯一性；
- OpenCode fallback 和禁用环境变量；
- `unmanaged` 不生成 binding/materialization；
- management mode 原子转换；
- ownership/adoption；
- plan token、expiry、revision/hash CAS；
- empty target render 阻断；
- safe delete 只认 Hub rendered hash；
- aggregate 不统计 unmanaged/paused；
- projection 失败结果不 overstate success。

### 16.2 前端单元测试

- 状态 DTO 到单一主文案的映射；
- “由 Hub 管理”提交一个 plan/apply 动作，不拼两次 mutation；
- setup、collision、drift、detached 和 delete 完整动作矩阵；
- preview 前零 mutation；
- preview stale 后草稿保留；
- target card 不展示内部 ledger 字段；
- 中文 locale 英文句检测。

### 16.3 L2 隔离 HOME

新增稳定 evidence：

- `L2-AGENT-HUB-USER-INSTRUCTION-V2-001`：三 target inventory、优先级、fallback；
- `L2-AGENT-HUB-USER-INSTRUCTION-V2-PLAN-001`：preview/apply、CAS、ownership、零写入门闩；
- `L2-AGENT-HUB-USER-INSTRUCTION-V2-DELETE-001`：暂停、停止管理、单目标移除、tombstone 与恢复。

测试使用隔离的 `HOME`、`CODEX_HOME`、`CLAUDE_CONFIG_DIR`、`OPENCODE_CONFIG_DIR`、`XDG_CONFIG_HOME` 和 OpenCode compatibility 环境变量，不读取或修改开发者真实 home。

### 16.4 E2E

新增：

- `E2E-AGENT-HUB-USER-INSTRUCTION-V2-001`：首次设置主旅程；
- `E2E-AGENT-HUB-USER-INSTRUCTION-V2-CONFLICT-001`：collision/drift/detached；
- `E2E-AGENT-HUB-USER-INSTRUCTION-V2-DELETE-001`：危险动作与逐目标结果；
- `E2E-AGENT-HUB-USER-INSTRUCTION-V2-A11Y-001`：键盘、焦点、live region、中文文案。

E2E fixture 必须覆盖 Hub 未设置状态，不能只用默认已启用 fixture。

### 16.5 L3 真实 CLI

分别在 macOS、Windows、Ubuntu 的精确 CLI 版本执行：

- 路径发现；
- 优先级；
- 文件写入/删除/恢复；
- 新 session/重启生效；
- 外部编辑与 CLI 实际读取结果。

建议证据：

- `L3-AGENT-HUB-USER-INSTRUCTION-CLAUDE-001`
- `L3-AGENT-HUB-USER-INSTRUCTION-CODEX-001`
- `L3-AGENT-HUB-USER-INSTRUCTION-OPENCODE-001`

未执行时必须保持 `NOT VERIFIED`，support manifest 中对应 write/remove capability 继续 blocked。

## 17. 实施阶段

### Phase 0：正确性与止血

- 修复 readOnly/unsupported 和 absent/verified 错误组合；
- 新增 `unconfigured`；
- 页面隐藏无法解释的内部 badge；
- 将“允许写入”改为“后端连接正常”；
- 阻断旧 UI 对 user instruction 的危险 mutation；
- 补中文英文泄漏检查。

### Phase 1：Inventory 与 resolver

- 新增独立用户级 inventory；
- 补齐 Codex source chain；
- 补齐 OpenCode fallback/env detection；
- 返回 active/shadowed/ownership/capability；
- 迁移旧 absent binding 为真正 unmanaged。

### Phase 2：V2 用户界面与草稿

- 重组 Agent Hub 信息架构；
- 交付用户级指令专用 view；
- 交付首次设置向导、target cards 和公共/专属编辑器；
- 保留 advanced ledger；
- scan-only 状态下完成只读旅程。

### Phase 3：安全 Preview / Apply

- plan token、CAS、ownership/adoption；
- 原子 management intent；
- 路径/diff/优先级影响；
- 逐 target projection result；
- 完成真实 target evidence 后按 manifest 逐项开放写入。

### Phase 4：冲突、删除与历史恢复

- collision 三选一；
- drift/detached；
- 暂停、停止管理、单 target 移除；
- canonical tombstone、history 和 restore。

### Phase 5：LAN/Git 收敛

- 区分 canonical import 与 local activation；
- 新设备本机 setup；
- 收敛 legacy Claude 文件同步写入链路；
- 更新质量矩阵和事实文档。

## 18. 完成定义

只有同时满足以下条件，才能将本 Spec 标记为“已交付”：

1. 旧截图状态无法再由合法 V2 DTO 组合产生；
2. 用户能在不理解 canonical、binding、materialization 的情况下完成首次设置；
3. 三个 target 的实际生效来源、路径和优先级解释正确；
4. 所有写入和删除都经过预览、expected hash 与 ownership 校验；
5. unmanaged target 不参与健康聚合；
6. 编辑失败和 preview 过期不丢草稿；
7. 停止管理、暂停、单目标移除和 canonical 删除各自有独立合同；
8. LAN/Git 导入不自动激活本机 target；
9. L1/L2/E2E 全部通过；
10. 对外宣称可写的每个 target/平台/版本均有对应 L3 evidence；其余保持 scan-only 或 blocked；
11. `docs/prd.md`、support manifest、quality matrix、相关前后端分层指令和测试清单已同步；
12. 没有恢复旧 Claude-only 双写作为绕过 V2 安全门闩的路径。
