# Agent Hub 用户级镜像（Pull / Push）

- 日期：2026-08-23
- 状态：已确认（用户决策，待实现）
- 上位文档：
  - [`2026-08-08-agent-hub-three-phase-refactor-design.md`](./2026-08-08-agent-hub-three-phase-refactor-design.md)
  - [`2026-08-07-agent-hub-portable-asset-management-parity-design.md`](./2026-08-07-agent-hub-portable-asset-management-parity-design.md)
  - [`2026-08-04-agent-hub-user-instruction-management-v2-design.md`](./2026-08-04-agent-hub-user-instruction-management-v2-design.md)
  - [`2026-08-10-agent-hub-correction-design.md`](./2026-08-10-agent-hub-correction-design.md)
- 身份目录：[`docs/development/adapt-new-agent.md`](../../development/adapt-new-agent.md)

## 1. 文档地位

本 Spec **覆盖** 生产 UI 中 Agent Hub 的逐项 Pull 与选择式 LAN Push。它不推翻 Canonical Hub、Revision DAG、CAS、SnapshotEnvelope、portable-store、固定 LAN 无鉴权边界。

冲突优先级：

1. 本补丁中的用户确认决策
2. 本补丁
3. 2026-08-10 纠正规格里「Pull / Push 仍是勾选复制、原生安装受 scan-only 门禁」的条款
4. 2026-08-08 三阶段补丁 D3（同 Agent 用户主动 pull/push；跨 Agent 远端互传禁止）—— **同名 Agent 对号入座仍有效**；「勾选单条」被本补丁取代
5. 更早领域 Spec / Gate Plan

生产入口仍是 `/agent-hub` 用户级壳层的 Pull / Push。项目级、Git 导入、选中对端后的就地编辑，不在本次范围。

## 2. 已确认决策

| # | 决策 | 说明 |
|---|------|------|
| M1 | 镜像对齐 | 目标用户级变成源的完整副本。源有的覆盖；目标多出来的同类资产删除或停用。 |
| M2 | 一次全部已登记 Agent | 一次处理 catalog 中全部 Hub Agent（当前：`claude` / `codex` / `opencode` / `grok` / `gemini` / `cursor` / `pi`）。同名对号入座，禁止跨 Agent 翻译。 |
| M3 | 立刻写盘 | 成功项同时更新 Hub 账本与该 Agent 会写的用户级原生文件 / 配置 leaf。 |
| M4 | Pull 与 Push 都改 | Pull：对端覆盖本机。Push：本机覆盖所选对端。不再勾选单条，不再选 full/user/project/assets。 |
| M5 | 新操作，不打补丁 | 领域操作 `user_mirror`。新能力 token 与路由同发。旧逐项逻辑不作成功回落。 |
| M6 | 部分成功不回滚 | 单目标机内按 Agent / 条目继续；已成功保留。同 `clientRequestId` 重放结果；新跑新 id。Push 多对端互不影响。 |
| M7 | 强制预览 + 破坏性确认 | 必须 preview 后 apply。确认框勾选「将覆盖原生文件并删除目标多出的用户级资产」。 |
| M8 | 用户级 only | 项目级镜像不做。Workbench 项目控制台仍不提供 Pull/Push。 |

## 3. 用户结果

完成后，用户能够：

- 在用户级 Agent Hub 点 Pull，选一台在线对端，预览后把对方**全部已登记 Agent** 的用户级提示词（三槽 + 已写入的用户级文档）和 Skill / Command / Plugin / MCP **整份覆盖到本机**。
- 点 Push，选一台或多台对端，把**本机**同一范围整份覆盖到对方。
- 在预览里按 Agent 看到将写入、替换、删除的文件/资产数量，以及「含凭据」条数；看不到 secret 原文。
- 缺 `agent-hub.user-mirror.v1` 或预览过期时，操作失败且不会误走旧逐项 Pull/Push。
- 部分 Agent 失败时看到分项结果，已成功项不会被撤回；可再跑一次镜像。

## 4. 范围与身份

### 4.1 包含

对 catalog 每个 `AgentId`，用户级：

1. **提示词三槽** Hub canonical（公共 / 适配 / 独有）。
2. **用户级原生提示词文档**：该 Agent 已有读写合同的文件（如 Claude `CLAUDE.md`、各家配置目录下的 `AGENTS.md`、Gemini `GEMINI.md`，以及会物化的适配/独有槽文件）。
3. **Portable**：Skill、Command、Plugin、MCP 的用户级库存。

### 4.2 不包含

- 项目级指令、项目级 portable、Workbench 项目资产。
- 跨 Agent 适配 / 翻译 / full apply。
- Git device-lane 导入。
- 选中对端后的就地 inspect/save（`user-instructions.v1` / `portable-user.v1`）——仍是远程改对方，不是复制。
- 整份 home 目录 rsync、`.claude` 下与 Hub 无关的随意文件。
- LAN 鉴权、把 peer 称为已认证设备。
- spawn 未认证 CLI 做 Enable/Disable/Uninstall。

### 4.3 路径与共用文件

- 按 `TargetPathResolver` 解析的**规范路径**去重。同一路径只写一次。
- **禁止**把 A Agent 的 fallback 路径当成 B 的输出（OpenCode 缺自身 `AGENTS.md` 时回退 Claude `CLAUDE.md`：镜像 OpenCode 不得覆盖 Claude 的文件）。
- 公共槽不物化的身份（Grok / Cursor 读仓库 `AGENTS.md`）**不写**那份公共 `AGENTS.md`；只覆盖其适配/独有槽落点。
- Hub 独有槽文件与「用户级原生文档」按现有 `user_instructions` / `native_files` 白名单，不新发明路径。

## 5. 架构

### 5.1 能力与版本

- 新 P2P 能力：`agent-hub.user-mirror.v1`，与本节路由同发。
- `AGENT_HUB_API_VERSION`：**4 → 5**。旧 GUI 不得对 new sidecar 发起镜像；新 GUI 不得把镜像交给 v4 sidecar。
- 对端缺 token：该次 Pull 或该 peer 的 Push **整次失败**，零请求打到旧 `portable-pull` / `agent-hub.v1` push 并宣称镜像成功。
- `expected-device` / `clientRequestId` 仍是绑定与幂等标签，**不是**身份认证。界面保留 LAN 无校验风险提示。

### 5.2 领域操作

单一操作 `user_mirror`，方向由谁是 apply 端决定：

| UI | Source | Destination（apply 端） |
|----|--------|-------------------------|
| Pull | 所选 peer | 本机 |
| Push | 本机 | 每个所选 peer |

选择是隐式的：`mode = userScopeAllAgents`。请求体没有 `inventoryItemIds`、没有 `SnapshotSelectionMode`、没有冲突策略（固定 replace + 删除多余）。

### 5.2.1 资产选择（2026-09-04 增补）

在「一次全部已登记 Agent」的镜像内，用户可以在预览后、应用前选择**要同步哪些资产**（默认全量；不带 selection 的请求行为完全不变）：

- **wire 契约**（camelCase）：`UserMirrorSelectionFilterDto { includeInstructions: bool = true, portableKeys: Option<Vec<{kind, nativeId}>> }`；挂在 `ApplyUserMirrorRequest.selection`（本机 apply）与 `UserMirrorPlanDto.selection`（plan 落库/push 对端传递）上，均 `#[serde(default)]` 缺省 None。
- **键跨 Agent 联动**：选择键是 `(kind, nativeId)`，同名 Skill 在多个 Agent 上算同一资产——选中即所有 Agent 上一起 upsert；未选中的资产 upsert 与 delete/disable **一并跳过**。
- **includeInstructions=false**：原生指令文件写与 Hub 三槽覆盖全部跳过；`portableKeys=null` 表示全部 portable。
- **双保险执行**：源端 freeze 前 `filter_inventory_for_freeze` 用裁剪副本打包（省传输）；apply 端 `filter_agent_plan_for_selection` 是**唯一权威过滤点**（per-agent 剔除未选中 upsert/delete/disable，关指令时跳过 native 写与三槽覆盖）。未选中条目即使被冻结进对象也不会落地。
- **stale 校验不破坏**：push 的 `local.inventory_snapshot_hash != plan.local_inventory_snapshot_hash` 与 pull `collect_source_objects` 的探测 inventory 比对仍基于**全量** inventory；selection 只影响裁剪副本与对象集，不改 inventory 身份 hash。
- **push fan-out 携带**：`request.selection` 在 claim 后合并进内存 plan，`dest_plan` 序列化传对端 prepare/commit，对端落库完整 plan JSON 后 dest apply 自动过滤。
- 前端：预览区新增「同步内容选择」——指令总开关（默认开）+ portable 勾选列表（按 `(kind, nativeId)` 去重、默认全选、全选/全不选快捷操作）；全部勾选且指令开时 apply **不带** selection（等价默认）。

### 5.3 传输

复用 CAS 分块（单 chunk ≤ 8 MiB，offset 续传，SHA-256 校验）。对象落磁盘 staging，禁止把整包载入内存。累计上限 **512 MiB**（现 portable-pull 的 64 MiB 不够覆盖全 Agent + Plugin）。超限 fail-closed：`USER_MIRROR_TRANSFER_LIMIT`。

源端（被拉或主动推的内容方）暴露：

1. `POST /api/agent-hub/user-mirror/inventory`  
   全 Agent 用户级**元数据**快照：槽 hash、原生文件 hash、portable identity/kind/nativeId/hashes、MCP `{present,hash}`。无 path、无 secret、无 env。
2. `POST /api/agent-hub/user-mirror/selection`  
   冻结 SnapshotEnvelope（指令槽 + 原生文件字节 + portable 对象）。源端不得因此 adopt/卸载任何本地资产。
3. `GET /api/agent-hub/user-mirror/objects/:transferId/:objectHash?offset=`  
   与现 portable-pull 相同的 octet-stream 合同。

目标端 apply 永远在 **owning destination process** 执行（导入 Hub → 写盘 → 删除多余 → rescan）：

- **Pull**：目标是本机，apply 只走 Tauri/control，不把 apply 再打到 LAN。
- **Push**：目标是 peer。源端把 envelope/objects 推到对端后，对端必须走新的 commit，而不是旧 `POST /api/agent-hub/push/commit`。新路由：
  - `POST /api/agent-hub/user-mirror/prepare`
  - `PUT /api/agent-hub/user-mirror/:transferId/objects/:objectHash`
  - `POST /api/agent-hub/user-mirror/:transferId/commit`  
    commit 成功 **包含** 原生写盘与多余删除；不得返回成功却只进 canonical。

旧 `agent-hub.v1` prepare/objects/commit 与 `portable-pull.v1` 三路由保留作 N/N+1，**生产 UI 不再调用**。

### 5.4 预览与幂等

- Preview 在 apply 端对比「源 inventory 快照」与「目标当前用户级扫描」，生成 plan（TTL **15 分钟**）。
- Plan 绑定：源/目标 inventory hash、peer id、catalog Agent 集合。任一侧漂移 → `USER_MIRROR_STALE`，必须重新预览。
- Apply 带 `planToken` + `clientRequestId`。同对同结果重放；同 id 不同 plan → 409。
- 崩溃后 `get(clientRequestId)` 可对账；未完成标 `outcomeUnknown`，并 best-effort 附带 rescan 观察。禁止把未知标成成功。

### 5.5 控制面

Owner/sidecar 新 op，GUI 经 loopback control 代理（与现 Agent Hub 一致）。Apply 墙钟 **900s**（全 Agent 写盘），UI 按 Agent 显示进度。禁止 GUI 直连 peer HTTP。

Tauri / control op 名称固定为：

- `agent_hub_preview_user_mirror`
- `agent_hub_apply_user_mirror`
- `agent_hub_get_user_mirror`

生产页删除逐项 Pull 与选择式 Push 入口（含 `agent_hub_preview_portable_pull` / `apply_portable_pull` / `push_selection`）。测试夹具与 E2E 改为镜像合同。

## 6. 落盘规则

镜像是**用户发起的一次性覆盖**，允许写原生用户级文件，**不**要求每条动作都有 L3 `activatePackage` evidence。硬限制：不 spawn 未认证 CLI；不 remap 到另一家 executor。Plugin 启停仍写 viewing 标记；MCP / 主指令文件走配置或原子文件写。本例外与「确认当前版本」「恢复为仓库资产」并列，写入 `adapt-new-agent.md`。

### 6.1 提示词

对每个 Agent：

1. 用源三槽 canonical 覆盖目标 Hub 三槽（空槽覆盖为空）。
2. 将源端**实际原生文件字节**写入目标对应白名单路径（CAS + 原子 rename）。不要在目标机用槽重新编译来「近似」源文件。
3. 源文件不存在或为空：目标该路径按现有 writer 清空或删除托管文件（与 `write_user_native_instruction_file` 空正文语义一致）。
4. 目标多出来、且落在该 Agent 白名单内、源快照没有的托管文件：清空。白名单外的磁盘文件不动。

### 6.2 Skill / Command

- 源有的：canonical 导入 + 安装到**该 Agent 自己的 native 根**（portable-store + 正规软链）。冲突一律替换。
- 目标多的：对该 viewing Agent **detach / 卸下 native**。共享仓库包只要仍被其他 Agent 使用则不得 `destroyStore`。禁止为列表干净去改所有者磁盘上的 `~/.agents` 源树。
- **源端先确认版本并收编进 portable-store**：freeze 打包前，源端经 `user_mirror/store_migration.rs::migrate_portable_assets_into_store` 先把本机 user-scope Skill/Command 收编进本机 portable-store——已在库（StoreLink）跳过；「仓库真树 + Agent 根软链」的逃逸链可解析时走 `MaterializeEscapeLink`（真树复制进 store、原软链位置换成 store 软链）；散落真树走 `MigrateToStore`（move 进 store、原位换链，同名冲突按 frontmatter version / mtime 裁决记 manifest 版本）；单条失败不阻断镜像。push 在 preview 与 apply(freeze) 前各执行一次（幂等）；pull 在对端 selection freeze 前与本机源分支执行。随后 freeze 发送 store 真树，对端先落对端 portable-store 再建软链。断链（软链目标缺失）不迁移，仍 fail-closed blocked；可解析但迁移失败的项由 `hash_skill_directory_dereferenced` 只读 dereference 打包兜底。Plugin/MCP 及 plugin 组件 Skill/Command 完全不动，按原文件覆盖同步。

### 6.3 Plugin

- 源有的：按 viewing Agent 写入启用标记，使目标该 Agent 的开关与源一致。
- 目标多的：**Disable**（viewing 标记），不在镜像里自动 Uninstall。Uninstall 仍走现有所有者磁盘规则与 UI。
- 禁止把 Enable/Disable 映射到另一家 CLI。

### 6.4 MCP

- MCP 不进 portable-store。覆盖该 Agent 配置 leaf 的 server 集合，**包括凭据明文**（只走 CAS 对象，inventory/UI/log 仍是 `present`+`hash`）。
- 目标 leaf 中源没有的 server **整项删除**（含其凭据）。
- `legacyLossy` 占位不得覆盖目标已有真凭据；该 server 标失败并继续其他项。

### 6.5 顺序与失败

单目标机建议顺序：转入 CAS → 按 Agent 导入指令槽 → 写原生提示词文件 → portable Skill/Command → Plugin 标记 → MCP leaf → 删除多余 Skill/Command → Disable 多余 Plugin → 删多余 MCP → 全量 user-scope rescan。

任一步失败：该 Agent/条目 `failed`，**已成功的不回滚**，继续其余 Agent。整次 `partial=true` 当且仅当存在失败或 unknown。全部成功才 `partial=false`。

## 7. 界面

用户级壳层仍是 Pull / Push 两个按钮（项目锁继续隐藏）。

**Pull 对话框**

- 只选源设备（在线 peer）。
- 去掉：Agent 切换、kind/scope 筛选、条目勾选、冲突策略。
- 「预览」后按 Agent 分组列出：提示词文件写/清空、资产新增/替换/删除、Plugin disable、MCP 删除、凭据条数。
- 必须勾选确认文案后才能「应用」。
- 忙时不可点遮罩/Escape 关闭。
- 结果区分 succeeded / failed / unknown；提供「核对」。

**Push 对话框**

- 只选对端（可多选）。去掉 full/user/project/assets 与手工 asset id。
- 预览、确认、凭据披露、LAN 风险提示与 Pull 相同。
- 每 peer 独立报告（与现 multi-target 一致）。

文案标明：同类 Agent 对号入座、将覆盖原生文件、将删除目标多出的用户级资产、LAN 无调用者身份校验。

## 8. 错误与注意力

稳定 code（写入协议与前端 decoder）：

| Code | 何时 |
|------|------|
| `USER_MIRROR_CAPABILITY_UNSUPPORTED` | 对端无 `agent-hub.user-mirror.v1` |
| `USER_MIRROR_PEER_OFFLINE` | 对端离线 |
| `USER_MIRROR_STALE` | inventory/plan 过期 |
| `USER_MIRROR_PREVIEW_REQUIRED` | 未预览或预览与当前选择不一致 |
| `USER_MIRROR_TRANSFER_LIMIT` | 超过 512 MiB |
| `USER_MIRROR_NATIVE_PATH_FORBIDDEN` | 解析到白名单外路径 |
| `USER_MIRROR_LEGACY_LOSSY_BLOCKED` | MCP 占位凭据 |
| `USER_MIRROR_PARTIAL` | 结果 DTO `partial=true`（不是 transport 错误） |

Push 单 peer 失败可进 Attention，稳定 id `agent-hub:mirror-failed:<requestId>:<peerId>`，只导航到 Agent Hub，Inbox 内不执行。摘要不含 payload/secret。

## 9. 文档与 N/N+1

实现时同步：

- `docs/prd.md` §2.5：Pull/Push 改为用户级全 Agent 镜像写盘，不再写勾选复制。
- `docs/p2p-protocol.md`：宣告 `agent-hub.user-mirror.v1` 与六条路由（源：inventory/selection/objects；目标：prepare/objects/commit）。旧 portable-pull 与 `agent-hub.v1` push 标注「非生产 UI」。
- `docs/development/quality-matrix.json`：新 L2/E2E id；双机 mDNS 保持 `NOT VERIFIED`。
- `docs/development/adapt-new-agent.md`：镜像写盘例外；新 Agent 必须进入镜像集合，缺席要显式 `unavailable` 而不是静默跳过。
- 根/`web`/`src-tauri` `AGENTS.md`：一句话指向本 Spec，不复制长表。

旧 peer：无 token 则镜像按钮对那台设备失败并提示升级。不得把旧逐项 Pull 当镜像成功。

## 10. 验收

| ID | 层 | 必须证明 |
|----|----|----------|
| `L2-AGENT-HUB-USER-MIRROR-001` | L2 | 双隔离 `data_dir`：全 Agent 用户级镜像；目标多余 Skill/MCP 消失；三槽 + 原生文件字节与源一致；Grok 不写公共 `AGENTS.md`；MCP UI 无 secret；缺能力零旧路由命中 |
| `L2-AGENT-HUB-USER-MIRROR-002` | L2 | 单 Agent 写失败 → partial、已成功 Agent 保留、同 `clientRequestId` 重放 |
| `L2-AGENT-HUB-USER-MIRROR-003` | L2 | dest extras：Skill detach、Plugin disable 而非 uninstall、MCP server 删除 |
| `E2E-AGENT-HUB-USER-MIRROR-001` | E2E | Pull/Push 无条目勾选与 mode radio；预览门闩；确认框未勾选不能 apply；stale 禁用 apply |
| `L3-AGENT-HUB-USER-MIRROR-001` | L3 | 双机 mDNS 真机 — 未跑则 `NOT VERIFIED` |

回归：现有 portable 本机 enable/disable、远端就地三栏、Git import、项目锁隐藏 Pull/Push。旧 `agent-hub-interaction` 里逐项 Pull 断言改为镜像或删除。

## 11. 实现切片（供后续 plan，不是本 Spec 的实现）

1. 协议 + capability + API version 5 + fail-closed 缺能力。
2. 源 inventory/selection builder（指令槽 + 原生文件 + 四类 portable）。
3. Destination apply：写盘规则 §6 + extras + rescan + 幂等 ledger。
4. Push：本机构建一次，每 peer dest-apply；生产 UI 停止旧 push。
5. 前端对话框替换 + i18n + E2E。
6. 文档与 quality-matrix。

每步可独立证明；未完成写盘不得宣称 M3。
