# Agent Hub 四类资产管理功能等价设计

**日期：** 2026-08-07

**状态：** 已确认，待实施计划

**适用范围：** Agent Hub 中 Claude Code、Codex、OpenCode 的 Skill、Command、Plugin、MCP 管理，以及同类 Agent 间的远端选择性 Pull

## 1. 背景

旧 `/claude-code` 页面已经实现 Claude Code 用户级资产的本机清单、搜索、类型与启用状态筛选、启停、卸载、局域网远端 inventory、选择性 Pull、overwrite 与逐项结果报告。Agent Hub 接管 `/claude-code` 路由后，这些实现仍保留在仓库，但页面不再可达；Agent Hub 的 Portable Assets 只展示已经进入 canonical ledger 的资产及三目标投影矩阵，无法承担本机实际库存管理。

当前断点包括：

- `/claude-code` 无条件重定向 `/agent-hub`，旧页面成为不可达代码；
- `agent_hub_list_assets` 只读取 Hub repository，不返回尚未纳管的本机原生资产；
- 旧 Claude mutation 在 Agent Hub 启用时 fail closed，不能作为隐藏的第二写入路径；
- `setTargetBinding` 已有 API/controller，但视图没有安装到缺失目标的入口；
- Plugin Drawer 展示 ownership-aware 删除预览，却没有确认执行闭环；
- Skill、Command、MCP 被错误地复用 Instruction Blocks 详情；
- 现有 E2E 主要验证 mock target matrix 与旧路由重定向，未锁定真实库存、启停、卸载和远端 Pull 的功能等价。

本设计直接在 Agent Hub 内实现功能等价，不恢复旧页面，也不设置过渡入口。

## 2. 已确认的产品决策

1. 采用“真实库存 + Hub canonical 对账”双层模型。
2. 恢复全部四类资产：Skill、Command、Plugin、MCP；界面按四类等权划分，不重点突出其中任何一类。
3. 覆盖 Claude Code、Codex、OpenCode 三个 Agent。
4. 本机管理覆盖用户级和已在 Workbench/Agent Hub opt-in 的项目级资产。
5. 未 opt-in 项目允许只读扫描与预览，不允许写入。
6. 不增加任意目录级写入入口；目录资产必须先进入项目映射或既有 scope 合同。
7. 恢复远端 inventory 与选择性 Pull，但只允许同类 Agent 互传：Claude→Claude、Codex→Codex、OpenCode→OpenCode。
8. Pull 后先导入本机 Hub canonical，再默认安装到同类本机 Agent；不支持远端不同 Agent 间转换或互传。
9. 不恢复旧 ZIP→Claude 直写为新 UI 的第二套同步真相；Pull 复用 Agent Hub SnapshotEnvelope、对象存储、导入器与幂等 ledger。
10. 不恢复旧 `/claude-code` 页面；新能力达到功能等价后删除旧前端页面和孤立 API。
11. 所有写操作仍由 sidecar owner 执行；GUI 不成为第二 watcher 或 writer。
12. 发现资产只建立 inventory 事实，不自动纳管、不自动取得 ownership、不自动修改 canonical head 或目标文件。

## 3. 目标与非目标

### 3.1 目标

- 用户打开 Agent Hub 即可看到三个 Agent 的真实四类资产库存，无需资产先进入 Hub ledger。
- 恢复并统一本机搜索、筛选、启停、卸载、结果报告。
- 提供明确的外部管理、Hub 管理、漂移、碰撞与不支持状态。
- 为四类资产提供符合其语义的专属详情和动作。
- 恢复远端设备 inventory、选择、冲突预览、同类 Agent Pull、逐项报告。
- 保持 Agent Hub 单一写入源、revision、ownership、冲突、Snapshot 与项目 opt-in 边界。
- 用真实文件/CLI shim L2 证据锁定功能等价，避免 mock UI 再次掩盖真实写入缺口。

### 3.2 非目标

- 不支持 Claude、Codex、OpenCode 之间的远端跨 Agent 转换。
- 不新增公共 marketplace/catalog 浏览或在线安装商店。
- 不把扫描到的所有资产自动纳入 Hub。
- 不统一三种 CLI 的全量配置文件，只管理本设计范围内的四类资产。
- 不改变固定 LAN 边界，不引入设备身份认证、可信设备或 capability token。
- 不宣称尚未执行的真实多机、全平台 CLI 写盘或 OpenCode TUI L3 已验证。
- 不在本轮重做用户级指令 V2、项目指令编译、Runtime provider 或 Orchestrator。

## 4. 总体架构

系统增加独立的 Portable Inventory 层，与现有 canonical ledger 并列：

```text
Target adapters / native scanners
  ├─ Claude portable inventory
  ├─ Codex portable inventory
  └─ OpenCode portable inventory
                │
                ▼
      PortableInventorySnapshot
                │
                ▼
 Inventory ↔ Canonical Reconciler
  ├─ unmanaged
  ├─ hubManaged
  ├─ drifted
  ├─ externalCollision
  └─ unsupported
                │
                ├─ read model → Agent Hub UI
                └─ preview/apply action planner → sidecar owner
```

Inventory 是目标文件和 CLI 的当前事实；canonical/binding 是 Hub 的持久意图。两者不得互相冒充：

- `actualEnabled` 只能来自扫描结果；
- `desiredEnabled` 只能来自 Hub binding；
- `installed` 不能根据 desired presence 推断；
- `synced` 不能在未重新扫描验证时声明；
- inventory refresh 不创建 revision 或 binding。

## 5. Portable Inventory 合同

### 5.1 快照

新增 `PortableInventorySnapshotDto`：

```ts
interface PortableInventorySnapshotDto {
  inventorySnapshotHash: string;
  refreshedAt: string;
  stale: boolean;
  targets: PortableInventoryTargetDto[];
  items: PortableInventoryItemDto[];
}
```

`inventorySnapshotHash` 必须覆盖：

- target executable、version、config root 与 adapter capability；
- scope、项目映射和 opt-in 状态；
- 每项原生 ID、实际路径、内容/tree hash、启用状态与来源；
- canonical match、ownership、binding/materialization 摘要；
- Plugin package/component 关系；
- 影响动作许可的 warnings 与 collision 状态。

任何相关事实变化都令旧 preview 失效。

### 5.2 Target DTO

```ts
interface PortableInventoryTargetDto {
  target: 'claude' | 'codex' | 'opencode';
  installed: boolean;
  version: string | null;
  executable: string | null;
  configRoot: string;
  scanCapability: 'supported' | 'readOnly' | 'blocked';
  mutationCapability: 'supported' | 'previewOnly' | 'blocked';
  reasonCode: string | null;
  evidenceIds: string[];
}
```

### 5.3 Item DTO

```ts
type PortableAssetKind = 'skill' | 'command' | 'plugin' | 'mcp';
type PortableInventoryManagementState =
  | 'unmanaged'
  | 'hubManaged'
  | 'drifted'
  | 'externalCollision'
  | 'unsupported';

interface PortableInventoryItemDto {
  inventoryItemId: string;
  target: 'claude' | 'codex' | 'opencode';
  kind: PortableAssetKind;
  nativeId: string;
  displayName: string;
  description: string | null;
  version: string | null;
  scopeId: string;
  scopeKind: 'user' | 'project';
  projectId: string | null;
  projectOptedIn: boolean;
  sourcePath: string | null;
  sourceOrigin: 'standalone' | 'pluginComponent' | 'nativeConfig';
  parentPluginInventoryItemId: string | null;
  actualEnabled: boolean | null;
  contentHash: string | null;
  treeHash: string | null;
  canonicalAssetId: string | null;
  canonicalRevisionId: string | null;
  managementState: PortableInventoryManagementState;
  desiredPresence: 'present' | 'absent' | null;
  desiredEnabled: boolean | null;
  materializationStatus: string | null;
  capabilities: PortableInventoryItemCapabilitiesDto;
  warnings: string[];
}
```

`inventoryItemId` 由 target、scope、本机规范化 source identity 和 native ID 稳定派生；不得使用展示名作为身份。

### 5.4 四类扫描规则

#### Skill

- 扫描 native/compatibility/legacy standalone 目录以及 Plugin 内 Skill。
- 目录树必须包含 `SKILL.md`；内容与 supporting files 按 tree hash 识别。
- standalone 与 Plugin component 保持独立身份；UI 可关联但不得静默合并。
- active/disabled 路径必须映射为实际启用状态，不以 Hub binding 猜测。

#### Command

- 扫描目标 Agent 的原生命令文件或受支持的兼容表达。
- 保存 canonical name 与目标实际 invocation，禁止把适配后的 Skill 当成原生 slash command 显示。
- 无原生 enable 语义的 target 返回 `actualEnabled=null` 并禁用启停动作。

#### Plugin

- Claude/Codex 优先使用官方 CLI inventory；必要时结合 install path 和 manifest。
- OpenCode 扫描官方 Plugin 文件/目录与配置启用事实。
- Plugin 作为 package 展示；components/residuals 在详情中展开。
- component inventory 记录 parent plugin，但不与同名 standalone 资产合并。

#### MCP

- 扫描受支持的用户级与项目级 native config。
- inventory 只返回凭据是否存在、hash 和诊断，不返回 secret 原文。
- JSON/TOML/JSONC 无法解析时保留原文件，item/target 标记 blocked，不用空配置覆盖。

## 6. Canonical 对账与 Ownership

`PortableInventoryReconciler` 使用 target、scope、origin namespace、logical key、source identity 和内容 hash 对账。

结果规则：

- 没有 canonical match：`unmanaged`；
- Hub ownership 与 observed hash 一致：`hubManaged`；
- Hub ownership 存在但 observed hash 偏离 applied revision：`drifted`；
- 同一 materialized identity 被不兼容的外部 source 占用：`externalCollision`；
- adapter 或版本不支持所需语义：`unsupported`。

对账不得：

- 因同名自动合并 standalone 与 Plugin component；
- 因扫描结果缺失自动 tombstone canonical；
- 因 desired presence 推断本机文件存在；
- 修改 adoption、binding、revision 或 materialization 表。

## 7. 本机动作合同

### 7.1 API

新增：

```text
agent_hub_inspect_portable_inventory
agent_hub_preview_portable_asset_action
agent_hub_apply_portable_asset_action
agent_hub_get_portable_asset_action
```

动作类型：

```ts
type PortableAssetActionKind =
  | 'adopt'
  | 'enable'
  | 'disable'
  | 'uninstall'
  | 'installToSourceTarget';
```

Preview 请求必须携带：

- `inventorySnapshotHash`；
- `inventoryItemIds`；
- action kind；
- uninstall 的 `keepData`；
- overwrite/conflict policy；
- 可选 expected canonical revision。

Apply 请求只接受短期 `planToken` 和非空 `clientRequestId`。计划包含 target、scope、expected source hash、文件/CLI 动作、ownership 影响、canonical 影响、备份与回滚策略。

### 7.2 动作流程

```text
inspect → preview（零写入）→ 用户确认 → claim plan
→ 执行目标 adapter → 写 action ledger → 重新扫描
→ 比较预期与实际 → 返回逐项结果
```

结果状态统一为：

- `succeeded`
- `skipped`
- `failed`
- `blocked`
- `outcomeUnknown`

`outcomeUnknown` 必须先通过 `clientRequestId` 查询 ledger 和重新扫描对账，不盲目重放。

### 7.3 未纳管资产

用户对未纳管资产执行启停或卸载时：

- 不强制先 adoption；
- Hub 记录受控 action ledger 与审计元数据；
- 不创建长期 canonical ownership；
- 仍需 inventory hash、preview、确认和 post-action rescan。

只有显式“纳入 Agent Hub”才创建 canonical、revision、binding 和 ownership。

### 7.4 删除语义

- 单目标 uninstall/remove 不删除 canonical。
- Skill/Command 文件删除前写入 cc-partner 可恢复备份。
- MCP 使用 ownership-aware semantic patch，保留 unmanaged 字段和注释。
- Plugin preview 必须列出独占 component tombstone、共享/standalone component preserve、residual 和 keep-data 影响。
- source hash 变化、未知文件、ownership 不足或 destination collision 一律 fail closed。
- `delete everywhere` 只存在于资产详情危险区，不作为列表常驻动作。

## 8. 远端选择性 Pull

### 8.1 用户流程

```text
选择远端设备
→ 选择来源 Agent
→ 加载远端 inventory
→ 按四类/作用域/状态搜索筛选并勾选
→ Pull preview
→ 用户确认
→ 传输缺失 objects
→ 导入本机 canonical
→ 安装到同类本机 Agent
→ 重新扫描并返回逐项报告
```

### 8.2 同类 Agent 限制

- Claude source 只能选择本机 Claude；
- Codex source 只能选择本机 Codex；
- OpenCode source 只能选择本机 OpenCode；
- 请求、preview 与服务端 commit 均验证 sourceTarget == destinationTarget；
- 不提供跨 Agent target picker，不在后台自动适配到其它 Agent。

### 8.3 Scope 与项目映射

- 远端用户级资产导入本机用户级 Hub scope，并默认安装到同类用户级 target。
- 远端项目级资产只有在 Hub project identity 已映射到本机项目时才允许安装。
- 项目未映射时允许导入 canonical backup，但结果为 `importedCanonicalOnly`，不得猜测路径或自动 opt-in。
- 本机项目未 opt-in 时 preview 必须显示只导入 canonical，不执行目录写入。

### 8.4 冲突策略

替代旧布尔 overwrite，使用显式策略：

```ts
type PortablePullConflictPolicy = 'skipExisting' | 'replaceAfterPreview';
```

- `skipExisting`：相同 identity 已存在时逐项 skipped；
- `replaceAfterPreview`：preview 必须显示 canonical revision、目标文件与 ownership diff；
- stale preview、hash 不匹配或 external collision 不执行 replace；
- Plugin component 冲突按 component identity 与 ownership 逐项判定，不能整包粗暴覆盖。

### 8.5 传输与幂等

Pull 使用现有 SnapshotEnvelope/CAS：

- 远端 inventory 只传元数据；
- preview 固定 selection manifest 与 source device；
- object 传输按 hash 去重并支持 offset 续传；
- commit 使用 `(requestingDeviceId, clientRequestId)` 幂等；这些字段只用于路由绑定与幂等，不是身份认证；
- MCP 凭据在 Snapshot/Object 中保留原字节；UI、日志、错误和诊断继续脱敏；
- 旧 `legacyLossy` placeholder 不得覆盖 canonical credential；对应 item blocked 并要求从有原值的源重新 Pull。

## 9. Agent Hub 页面设计

### 9.1 一级工作区

Agent Hub 导航调整为：

1. 用户指令
2. Agent 资产
3. 项目配置
4. 同步与导入
5. 诊断

`/claude-code` 继续重定向，但目标为 `/agent-hub?section=assets&target=claude`。

### 9.2 四类划分

Agent 资产使用固定四类 tab/segmented control：

- Skill
- Command
- Plugin
- MCP

四类等权展示，共用：

- 搜索；
- Agent 筛选：全部/Claude/Codex/OpenCode；
- scope 筛选：用户级/项目；
- 状态筛选：全部/启用/禁用/异常；
- 管理筛选：全部/外部管理/Hub 管理/漂移/碰撞。

筛选消费 inventory read model，不向后端传任意 kind/scope 自由文本。

### 9.3 列表信息层级

列表主体展示真实事实：

- 名称、类型、Agent、scope；
- 实际启用状态；
- 来源、版本、路径摘要；
- 外部/Hub ownership；
- warnings 与 blocking reason；
- 与当前状态对应的单一主动作。

Hub desired presence、revision、materialization matrix、component ownership 与完整诊断下沉到详情。

### 9.4 四类详情

- Skill：目录树、`SKILL.md` 摘要、supporting files、standalone/plugin component、canonical/materialized invocation。
- Command：原生 ID、source file、目标调用形式与兼容性。
- Plugin：package 元数据、组件树、residual、activation、target 状态、ownership-aware delete preview。
- MCP：server 名称、transport 类型、配置来源、凭据存在性、目标配置 patch 预览；不显示 secret 原文。

所有危险动作使用共享 Dialog 预览确认，不使用 `window.confirm`。Views 不 import `@/api/*`；controller 持有请求序列、stale 防护、草稿与动作状态。所有 hooks 位于 early return 前。

### 9.5 同步与导入

保留现有 source-push 与 Git import，同时新增“从远端设备拉取”入口。Pull Drawer 复用四类筛选和选择模式，显示：

- 设备与来源 Agent；
- 远端 refresh/stale 状态；
- 选中项和预计 object 体量；
- scope/project mapping；
- conflict policy；
- preview 与逐项 apply/progress/report。

## 10. 错误、恢复与并发

稳定错误至少包括：

- `PORTABLE_INVENTORY_STALE`
- `PORTABLE_SOURCE_CHANGED`
- `PORTABLE_CLI_UNAVAILABLE`
- `PORTABLE_CLI_VERSION_UNSUPPORTED`
- `PORTABLE_TARGET_MISMATCH`
- `PORTABLE_PROJECT_MAPPING_REQUIRED`
- `PORTABLE_PROJECT_NOT_OPTED_IN`
- `PORTABLE_OWNERSHIP_CONFLICT`
- `PORTABLE_EXTERNAL_COLLISION`
- `PORTABLE_CREDENTIAL_LEGACY_LOSSY`
- `PORTABLE_ACTION_OUTCOME_UNKNOWN`
- `PORTABLE_PULL_PARTIAL`

恢复规则：

- inventory 刷新失败时保留上次 snapshot 并标 stale；stale snapshot 禁止 mutation；
- preview 失败保留筛选、选择和冲突策略；
- apply partial 返回每项状态，不把部分成功压成全成功；
- cancellation 只停止尚未 claim/执行的项，已执行项通过 ledger 和 rescan 对账；
- 同一 item 的非交换动作串行；不同 target/scope 且 write set 不重叠的动作可并行；
- watcher、手动 refresh 与 apply rescan 使用 generation/sequence，旧响应不得覆盖新快照；
- GUI/backend version 不兼容时保持只读 inventory，所有 preview/apply fail closed 并提示升级。

## 11. 安全与隐私

- 固定 LAN 无身份鉴权边界保持不变；UI 必须继续披露同一可达网络设备可读写执行的风险。
- Host、Origin、Content-Type、ConnectInfo、expected-device 与资源上限继续用于边界和路由校验，不得称为身份认证。
- inventory、日志、错误和 UI 不包含 MCP secret 原文。
- Snapshot、CAS、LAN 和 Git 中的 MCP 凭据保持原字节；产品不得宣称 secure erase。
- 路径展示允许用户查看本机来源，但日志和 doctor 继续归一 home 并脱敏非产品工程路径。
- 所有 archive/tree import 拒绝绝对路径、`..`、symlink、hardlink 与超出资源上限的内容。

## 12. 兼容与清理

### 12.1 路由

- 不恢复旧页面；`/claude-code` deep link 到 Agent Hub 的 Claude 资产筛选。
- `/claude-md` 继续进入用户指令工作区，不纳入本设计的资产管理范围。

### 12.2 旧前端

功能等价验证通过后删除：

- `web/src/pages/ClaudeCodeAssets/`；
- `web/src/api/claudeCodeAssets.ts`；
- 只被旧页面使用的 `ClaudeAssetRow`、`RemoteAssetPicker` 与旧筛选 helper；
- 对应孤立 i18n namespace 和测试。

删除前必须确认新页面覆盖旧四类筛选、启停、卸载、远端 inventory、选择性 Pull 和逐项报告。

### 12.3 旧后端与 P2P

- 旧扫描/mutation helper 在 target adapter 新路径完成前保留并重构复用。
- 旧 invoke mutation 不重新暴露给新 UI。
- 旧 P2P inventory/bundle 在 N/N+1 兼容窗口保留，但不计入新 Hub Pull 成功状态。
- 最早删除旧路由时继续遵守 runningVersion、earliestRemovalVersion 与稳定迁移 evidence 合同。

## 13. 测试策略

### 13.1 Rust 单元测试

- 三 target × 四 kind scanner 正常、缺失、损坏与版本不支持；
- inventory identity/hash 稳定性；
- standalone 与 Plugin component 不合并；
- canonical reconciliation 五种状态；
- action preview 零写入与 stale hash 拒绝；
- plan token、clientRequestId 幂等与 outcomeUnknown 对账；
- Plugin ownership-aware 删除；
- MCP semantic patch 保留 unmanaged 字段/注释；
- target mismatch 与 project mapping/opt-in 门闩；
- Pull conflict policy、partial、resume 和 credential legacy-lossy。

### 13.2 L2 集成测试

- 使用隔离 home/data dir 和 CLI shim 验证 Claude/Codex/OpenCode inventory；
- 启用、禁用、卸载后真实文件/CLI inventory 变化；
- Skill/Command 备份与恢复；
- Plugin package/component 删除不误伤共享引用；
- 两个本地 mock peer 完成同类 Agent inventory→selection→object transfer→canonical import→target install→rescan；
- 重复 `clientRequestId` 不重复执行；中断后按 ledger/object offset 恢复；
- 未映射/未 opt-in 项目只导入 canonical，不写项目目录。

### 13.3 前端单元与 E2E

- 四类划分与所有筛选组合；
- inventory stale 保留和 mutation 禁用；
- 四类专属详情；
- preview/apply/partial/outcomeUnknown UI；
- absent target 安装入口；
- Plugin 删除预览确认闭环；
- `/claude-code` deep link 到 Claude 资产视图；
- 远端设备、来源 Agent、同类限制、选择、冲突策略、progress 与逐项报告；
- E2E mock 继续用于 UI 确定性，但不得替代 L2 真实文件/CLI evidence。

### 13.4 文档与门禁

- 更新 `docs/prd.md` 的 Agent Hub 与 Claude Code 资产章节；
- 更新 `docs/p2p-protocol.md` 路由与 capability；
- 更新 `docs/development/quality-matrix.json`，为本机 parity 与同类 Pull 增加稳定 evidence ID；
- 更新根 `AGENTS.md` 组件清单和 `web/CLAUDE.md` controller/view 合同；
- 运行 route inventory、quality traceability、docs self-test 与 docs check。

## 14. 实施拆分

本设计拆为两个实施 Plan：

1. **Portable Asset Inventory、Action Plan 与 Same-Agent Pull 后端计划**

   负责 inventory/reconciliation DTO、target scanner 接线、preview/apply/ledger、Snapshot Pull、control/IPC/P2P、Rust/L2 证据。
2. **Agent Hub 四类资产管理 UI 与兼容清理计划**

   消费 Plan 1 的稳定合同，负责 API decoder、controller/views、四类详情、远端 Pull UI、路由、旧前端删除、E2E、PRD/指令文档收尾。

Plan 2 依赖 Plan 1 的 DTO、命令和测试 fixtures。Plan 1 不依赖 Plan 2，可先独立完成并通过服务层/L2 验证。

## 15. 完成合同

只有同时满足以下条件，才能宣称本设计完成：

1. 三 Agent 四类真实库存可发现，尚未纳管资产可见。
2. 用户级与项目级 scope 遵守 project mapping/opt-in 边界。
3. 本机 enable/disable/uninstall 经 preview/apply，完成后由 rescan 验证实际状态。
4. Skill/Command/Plugin/MCP 各有符合自身语义的详情与动作。
5. Plugin package/component 不重复展示，删除不误伤共享/standalone component。
6. 同类 Agent 远端 Pull 支持 inventory、选择、冲突预览、对象传输、canonical import、同类 target install、幂等和逐项报告。
7. 不存在远端跨 Agent 互传入口或后端绕过。
8. `/claude-code` deep link 到新版 Claude 资产视图；旧前端页面/API/组件仅在 parity evidence 后删除。
9. Rust unit、L2、前端 unit/build/E2E 和文档门禁使用新鲜命令通过，并记录精确结果。
10. 未执行的真实多机/全平台 CLI/L3 项继续标记 `NOT VERIFIED`；mock/L2 不升格为 L3。
11. 无第二 writer、无自动 adoption、无 silent overwrite、无凭据日志/UI 泄露。

## 16. 相邻竞态检查清单

每次计划级修复和复审必须覆盖：

- refresh/apply/rescan 的旧响应覆盖与 generation 顺序；
- 同一 item 并发 enable/disable/uninstall 的串行化；
- preview 后 source/canonical/project mapping/CLI version 变化；
- retry、clientRequestId 幂等和 outcomeUnknown 对账；
- Pull cancellation、对象续传、commit 重放与 partial report；
- watcher 与手动动作同时修改 target 文件；
- Plugin shared component 引用与 package 删除竞态；
- MCP config semantic patch 与外部编辑竞态；
- GUI/backend 版本切换和 owner restart；
- LAN expected-device mismatch、资源上限与 legacyLossy credential。
