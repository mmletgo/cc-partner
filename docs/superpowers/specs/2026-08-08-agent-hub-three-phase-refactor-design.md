# Agent Hub 三阶段重构 — 产品决策补丁

- 日期：2026-08-08
- 状态：已确认（用户决策）
- 上位文档：
  - [`2026-07-29-multi-cli-agent-hub-design.md`](./2026-07-29-multi-cli-agent-hub-design.md)
  - [`2026-08-04-agent-hub-user-instruction-management-v2-design.md`](./2026-08-04-agent-hub-user-instruction-management-v2-design.md)
  - [`2026-08-07-agent-hub-portable-asset-management-parity-design.md`](./2026-08-07-agent-hub-portable-asset-management-parity-design.md)
- 执行大纲：session plan `Agent Hub 全面重构规划`（三阶段 P1–P3）

## 1. 文档地位

本 Spec **不推翻** Canonical Hub、Revision DAG、CAS、Inventory 双层模型、SnapshotEnvelope、固定 LAN 边界等基础架构。  
它记录 2026-08-08 用户确认的四项产品决策，并**覆盖**旧文档中与之冲突的条款。

冲突优先级：

1. 用户本补丁中的确认决策  
2. 本补丁  
3. 2026-08-04 / 2026-08-07 领域 Spec  
4. 2026-07-29 Multi-CLI 总 Spec  
5. 历史 Gate Plan / handoff

## 2. 已确认决策

| # | 决策 | 说明 |
|---|------|------|
| D1 | 三端同级 | Claude Code、Codex CLI、OpenCode 均为交付目标，不缩成 Claude+Codex only |
| D2 | 阶段一必须真实写盘 | 结束条件含用户级/项目级 instruction 与 skill/command/mcp/plugin 的 preview→apply 原生写盘；非 scan-only 伪装完成 |
| D3 | 局域网 Pull + Push | 同 Agent 用户主动 pull 与 push；跨 Agent 远端互传禁止 |
| D4 | 同机跨 Agent 仅手动 | 用户选择源资产与目标 Agent，预览适配后确认写入；**不做** sidecar 后台跨 target 自动收敛 |

## 3. 三阶段产品合同

### 3.1 阶段一 — 本机真实管理

**范围：**

- 用户级 instruction（三端）  
- 项目级 instruction（Workbench 登记 + Hub opt-in；至少项目根）  
- Portable 四类：skill / command / mcp / plugin（三端等权）

**完成条件（摘要）：**

1. `support-manifest` 在已认证 CLI 版本上允许对应写能力；未认证版本/平台 fail-closed  
2. 用户级：discover → setup/update preview → apply 写盘 → rescan  
3. 项目级：opt-in preview → enable → 根指令读写；未 opt-in 禁止写  
4. Portable：真实 inventory + adopt/enable/disable/uninstall（及 plugin 删除预览）闭环  
5. 证据：L2 隔离 home/shim 必须通过；真机 L3 未跑保持 `NOT VERIFIED`，但不得在写能力已 Supported* 时缺失 matrix evidence ID

**非目标：** Git 自动 import 改造、Orchestrator runtime、marketplace 商店。

### 3.2 阶段二 — 局域网同 Agent 互传

**范围：**

- Pull：目标设备用户主动拉取远端 inventory 选中的 instruction + portable  
- Push：源设备用户主动推送到 1..N 在线设备  
- 仅 Claude→Claude / Codex→Codex / OpenCode→OpenCode  

**硬规则：**

- 跨 target 在 preview 与 commit 前失败  
- 未映射 project → `importedCanonicalOnly`，不猜路径、不自动 opt-in  
- 冲突策略：`skipExisting | replaceAfterPreview`  
- LAN 文案禁止「已认证/可信/安全设备」  
- MCP 凭据不进 DTO/日志/DOM  

**依赖：** 阶段一写盘已可用（否则装回本机仍 blocked）。

### 3.3 阶段三 — 同机跨 Agent 手动同步与适配

**范围：**

- 用户从源 Agent 选择 instruction 块或 portable 资产  
- 选择一个或多个目标 Agent  
- 系统给出 shared / adapted / targetOnly / residual 预览  
- 用户确认后执行**一次性** projection  

**相对 2026-07-29 的偏差（D4）：**

| 旧条款 | 本补丁 |
|--------|--------|
| 同机 sidecar 持续扫描并对账后自动投影到多 CLI | 默认 **关闭**跨 target 后台自动写文件 |
| 外部编辑触发三方 merge 后可能写回其他 target | 外部编辑仅更新 inventory/drift UI；跨 target 必须再经用户确认 |
| 同机自动收敛为默认体验 | 默认体验为「本机单 target 管理」+「手动跨 Agent 向导」 |

**仍允许：**

- 用户 apply 后的 **同 target** projection job 跑完（阶段一写盘完成态）  
- 后台 **扫描** inventory（只读）  
- 跨 target apply 失败/冲突进入 Attention  

**适配诚实性：**

- Instruction：CLI 专属术语块默认 targetOnly + needsAdaptation  
- Skill：尽量 portable；invocation 差异 adapted/alias  
- Command：无统一模型时不得伪装原生 slash command  
- MCP：字段子集映射；未知字段保留 raw；凭据原字节  
- Plugin：分解 capability；hook/runtime residual 不自动转码；partial 必须点名 blocker  

## 4. 写能力与证据合同（阶段一横切）

1. 唯一门闩：`src-tauri/src/agent_hub/support/support-manifest.json` + `evaluate_target_support`  
2. 将写能力标为 Supported* 时必须同时具备：  
   - 非空 `minTestedVersion` / `currentTestedVersion`  
   - quality-matrix 中存在的 `evidenceIds`  
   - 对应 L2 或已执行 L3 记录（L3 未执行则 status 仍为 `NOT VERIFIED`，但 ID 必须存在且 notes 诚实）  
3. 禁止：在 `baseline_write_capabilities_are_blocked` 式合同仍要求全 blocked 时，无配套测试与 checker 更新就放开写  
4. OpenCode 本机未安装时：可先保持写 blocked，或仅解锁文件级 projection（若 adapter 不依赖可执行写）；不得伪造 version 认证  

## 5. UI 信息架构（阶段边界）

| Section | 阶段一 | 阶段二 | 阶段三 |
|---------|--------|--------|--------|
| userInstructions | 完整可写 | — | 「同步到其他 Agent」入口 |
| projectInstructions | opt-in + 根指令可写 | 项目资产 LAN 映射诚实 | 同左 |
| assets（portable） | 四类本机管理 | Pull drawer | 跨 Agent 向导 |
| syncImport | 可隐藏 LAN 或诚实「待阶段二」 | Pull + Push 主入口 | 不承载跨 Agent |
| diagnostics | probe / support / 冲突 | 复制 ledger | 适配 partial blockers |

## 6. 明确不在本三阶段

- 新增 LAN 鉴权 / capability token  
- 后台跨 target 自动同步（已被 D4 否决）  
- 全量 home 目录镜像  
- 用 LLM 自由改写任意指令/可执行代码  
- 宣称未执行的多机/全平台 L3 已通过  

## 7. 迁移与兼容

- 不另起第二套 Hub；复用 Canonical + Portable Inventory  
- 旧 `/claude-code` 与 `claude_code_assets` 直写：Hub 启用时 fail-closed；功能等价 E2E 通过后删除孤立前端  
- 旧「仅 source-push」文案改为 Pull+Push；协议 N/N+1 facade 按现有 replication 模块演进  

## 8. 验收总表

| 阶段 | 最小可宣称完成 |
|------|----------------|
| 一 | 三端（或已认证子集+OpenCode 诚实 blocked）用户级 instruction 写盘；项目 opt-in 根指令写盘；四类 portable 至少 skill+mcp 每端一条 enable/disable/uninstall L2 |
| 二 | 同 Agent Pull skill+instruction；同 Agent Push 到第二 data-dir peer；跨 Agent 服务端拒绝 |
| 三 | 手动跨 Agent instruction 成功路径 + 专属术语隔离；Skill 至少一条；Plugin partial 诚实；负向测试锁定无后台跨 target 写文件 |

## 9. 修订记录

- 2026-08-08：初版，固化 D1–D4 与三阶段边界。
