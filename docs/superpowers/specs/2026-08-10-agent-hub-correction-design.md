# Agent Hub 安全与交互纠正规格

- 日期：2026-08-10
- 状态：implemented
- 适用版本：下一次 Agent Hub 修正发布
- 上位文档：[`2026-08-08-agent-hub-interaction-redesign-design.md`](./2026-08-08-agent-hub-interaction-redesign-design.md)
- 确认原型：[`2026-08-10-agent-hub-correction-prototype.html`](./2026-08-10-agent-hub-correction-prototype.html)

## 1. 文档地位

本规格纠正当前 Agent Hub 已暴露但尚未端到端成立的产品能力。与 2026-08-08 交互规格冲突时，以本规格为准；未被本规格覆盖的三槽 Canonical、Snapshot、固定 LAN 信任边界等合同继续有效。

本次修正的原则是：界面只承诺当前实现与证据能够兑现的动作；未知、过期、未认证或响应不完整一律不呈现为成功。

## 2. 问题与目标

### 2.1 已确认的问题

1. 未经真实 CLI L3 认证的跨 Agent 与资产写入口可以覆盖原生文件，且预览不展示真实内容差异。
2. 用户可选择项目、远端设备与全量适配，但这些路径在后端不可用或只是 stub。
3. 脏草稿、刷新、URL 历史与异步响应没有绑定同一上下文代次，可能丢稿、错写或把旧结果展示到新上下文。
4. 新库存与 legacy 矩阵并存，形成两套状态真源与不同安全门槛。
5. 关键错误、外部变化、保存状态和未知结果不够可见；Tab、库存行和冲突处理也不满足键盘与读屏合同。

### 2.2 用户结果

发布后，用户能够：

- 在唯一、可见的本机用户级上下文中查看 Claude、Codex、OpenCode 的 Agent 指令和四类资产库存。
- 安全编辑并保存 Hub 内的 Canonical 指令块，不把“保存到 Hub”误解成“写入 CLI 文件”。
- 从只读原始文件显式解析出 Hub 草稿，看到未保存、已保存、外部已变化与刷新失败状态。
- 在跨 Agent 页生成选择性适配预览并复制方案；不会在未认证时看到可点击的真实写盘按钮。
- 通过明确来源与目标的 Pull / Push 流程复制 Hub Snapshot；原生安装仍受 scan-only 门禁，过期数据或未知结果不会继续允许 Apply。
- 用键盘完成导航、打开详情和处理阻断对话框。

## 3. 发布边界与能力矩阵

| 能力 | 本次状态 | 用户可见行为 |
| --- | --- | --- |
| 本机用户级库存读取 | 可用 | 可选择 Agent 与五类 Tab，刷新并查看实际状态 |
| Hub Canonical 指令块保存 | 可用 | 按 revision CAS 保存；不写原生 CLI 文件 |
| 从原始文件解析块 | 可用 | 生成未保存 Hub 草稿，必须显式保存 |
| 原始 CLI 文件编辑 / 同步 | 阻止 | 原始文件只读；说明需真实 CLI 写盘认证 |
| Portable 原生启停、卸载、删除、投影 | 阻止 | 只展示库存与技术详情；mutation 不可用并展示稳定原因 |
| 项目级主上下文 | 未开放 | 不展示可选择的项目级 Tab；技术详情说明尚未接通 |
| 远端设备就地管理 | 未开放 | 不作为 Shell 上下文；LAN 仅通过 Pull / Push 任务进入 |
| 选择性跨 Agent 适配 | 预览可用 | 用户级、本机、单源；可复制方案，不写盘 |
| Claude 全量五类适配 | 未开放 | 不展示为可用模式；技术详情可标记实验 stub |
| LAN Pull / Push Hub Snapshot | 可用但显式 | 每次展示源、目标、范围、刷新时间和逐项结果；原生安装仍受写 capability 门禁 |

任何依赖真实 CLI 写盘的 capability 只有在对应平台、CLI 版本与当前构建的 L3 evidence 状态为 `VERIFIED` 且未过期时才可开放。仅存在 evidence ID 不构成认证。

## 4. 权威信息架构

```text
Agent Hub
├─ Agent：Claude | Codex | OpenCode
├─ 范围：本机 · 用户级（固定且始终可见）
├─ 主 Tab：Agent 指令 | Skill | 命令 | MCP | Plugin
├─ 次要任务：从设备拉取… | 推送到设备… | 适配到其他 Agent…
└─ 技术详情：CLI 探测、支持清单、诊断码（按需展开）
```

规则：

1. Shell 是 Agent、范围与主 Tab 的唯一上下文真源；库存区不得再提供独立 Agent 或 scope 筛选。
2. URL 只保存当前可兑现的 `agent`、`tab` 与可选资产详情；旧 `section`、`scope=project`、`deviceId` 深链进入后规范化到最近可用上下文，并显示一次说明。
3. `diagnostics` 不再作为隐藏正文分区；诊断只在“技术详情”中出现。
4. 所有改变写入目标的上下文始终可见。公共块表示“可供所有 Agent 复用”，当前 Agent 表示预览/原生来源目标，不能被隐藏。
5. 导航使用中性页签或单选分段控件；只有提交动作使用 primary 视觉权重。

## 5. Agent 指令工作区

### 5.1 三栏职责

| 栏位 | 权威内容 | 本次编辑能力 |
| --- | --- | --- |
| Hub 指令块 | Canonical common / adapted / target-only 块 | 可编辑、可保存到 Hub |
| 合成预览 | 当前块按 Agent 合成的结果 | 只读，实时更新 |
| 本机原始文件 | 当前 Agent 实际生效来源 | 只读，可复制、刷新、解析成 Hub 草稿 |

主动作必须使用无歧义文案：`保存 Hub 草稿`、`从原始文件解析为草稿`、`重新扫描原始文件`。不得再用“保存槽位”或未带目标的“同步”。

### 5.2 草稿状态

每份草稿绑定：

- `context`：Agent、用户级范围与指令 lane；
- `draftBaseRevision`：开始编辑时的 Canonical revision；
- `observedHeadRevision`：最近扫描看到的当前 head；
- `observedInventorySnapshotHash`：最近成功扫描的完整工作区快照，仅用于检测 source/inventory stale，不替代 Canonical CAS base；
- `blocksDirty`：Hub 块是否有未保存变更；
- `originalDirty`：本机原始文件栏是否存在独立草稿（兼容读取/解析流程，不由 Hub Save 消费）；
- `canonicalDrift`：Canonical head 是否在草稿期间变化；
- `sourceDrift`：原始来源 path/content 是否在草稿期间变化；
- `lastSavedAt` / `lastLoadedAt`：用户可见时间状态。

状态规则：

1. 无脏稿刷新：用最新数据重建工作区与保存基线。
2. 有脏稿刷新：保留草稿与 `draftBaseRevision`；只更新观测 head/snapshot 并分别计算 `canonicalDrift`、`sourceDrift`，禁止悄悄抬高 Canonical 保存基线。
3. 只有 `canonicalDrift` 阻止 `保存 Hub 草稿`；用户必须“重新载入并放弃草稿”或保留草稿查看变化。任何保存仍以原 `draftBaseRevision` 做 CAS，冲突后锁住后续保存，不能静默覆盖。
4. `sourceDrift` 只表示只读来源已变：不会触发或阻止 Hub Save，也不会清掉 Hub 草稿；但在显式重新载入确认新来源前，必须阻止原生 preview/apply。重新载入后再显式解析，才把当前来源变成 `blocksDirty=true` 的新草稿。
5. “从原始文件解析为草稿”只要内容发生变化，就设置 `blocksDirty=true`；保存成功后才变 clean。
6. 保存成功只消费请求开始时的 `blocksDirty`，并更新 Hub revision 与保存时间；保存期间的新编辑仍保持 dirty，`originalDirty`、原文内容与 `sourceDrift` 原样保留。

### 5.3 上下文切换

当存在脏草稿，任何 Agent、lane、Tab、URL history 或 deep-link 切换都先进入同一个阻断 Dialog：

- `保存并切换`：仅在有 `blocksDirty` 且无 `canonicalDrift` 时可用；Original-only 不得伪报保存成功；
- `放弃并切换`；
- `留在当前页面`（默认安全动作）。

确认完成前 URL 与正文继续保持旧上下文；不得出现“新标签 + 旧草稿”。Dialog 关闭后焦点回到触发控件。

### 5.4 状态反馈

- 工具栏持续显示 `已保存`、`有未保存更改`、`外部已变化` 或 `刷新失败，正在显示旧数据`。
- 成功使用 polite live region；阻断错误使用恰好一次 alert。
- 有旧数据时刷新失败必须保留内容并显示 stale banner，不能静默隐藏错误。

## 6. Portable 资产库存

1. observed inventory 是生产唯一真源；legacy target matrix 不再出现在主界面，也不能承担 mutation。
2. 库存筛选只保留搜索、资产状态与管理状态；Agent 与 scope 来自 Shell。
3. 行内提供明确的“查看详情”按钮；整行不承担鼠标专用点击。
4. 空态分为：确实未发现资产、筛选无匹配、扫描失败、正在加载。
5. 详情优先展示名称、类型、实际状态、来源与可行动问题；hash、native id、reason code 与原始 JSON 放入折叠“技术详情”。键值使用语义化定义列表。
6. Plugin 报告未加载或失败显示“未知 / 无法加载”，不得用 `0` 伪装。
7. 每次动作 Dialog 打开时按 `asset + action + session` 重置覆盖、保留数据等选项，不继承上一资产的危险选择。
8. 在写 capability blocked 时可以生成影响预览，但确认区明确标注“当前版本不会写入原生文件”，且无 Apply 按钮。
9. 资产 Action/Pull plan 必须绑定 Agent、scope、inventory query/snapshot、item/action、选择集与冲突策略；任一输入或 history context 变化立即推进 operation sequence 并清 plan。旧 preview/apply/reconcile 响应不得落入新上下文。
10. mutation affordance、planner、executor、Pull 与 projection scheduler 必须按实际动作检查精确 capability；原生普通资产写入只认 Render、原生 Plugin enable 只认 Activate、disable/uninstall 只认 Deactivate，Pull Plugin 安装同时要求 Render + Activate，Hub managed package 停用/Absent 只认 Deactivate；`PreviewOnly`、target 汇总状态或无关的已认证写能力不得替代该检查。

## 7. 跨 Agent 适配

### 7.1 本次可用流程

1. 源 Agent 来自 Shell，并持续显示；目标 Agent 必须显式选择且不含源。
2. 范围固定为本机用户级，不显示可确认但必失败的项目级选项。
3. 加载源正文、编辑、生成选择性预览；每个计划绑定 source、destination、scope、正文 hash 与上下文 generation。
4. 结果按“将创建 / 将更新 / 将跳过 / 无法适配”展示，内容差异必须展示真实 bounded diff 或前后内容摘要。
5. 用户可复制单项目标内容或整个方案；没有写入按钮。

### 7.2 响应诚实性

- source/context 变化时使 content 与 preview 都失效；destination/scope/body 变化只失效 preview，不能让仍有效的 source reload 永久停留在 loading。
- 每个异步提交必须同时校验当前 context fingerprint、operation sequence 与请求 input fingerprint；捕获值与自身比较不构成校验。
- 响应必须严格校验目标集合、枚举、结果基数与必填字段。
- 空结果、缺目标、畸形行或 lost ACK 表示“结果未知 / 响应无效”，不得显示绿色成功。
- 全量模式在五类生成器真实完成前不作为用户可选模式。

## 8. LAN Pull / Push

1. LAN 不成为 Shell 的浏览上下文；Pull / Push 是独立、可关闭的任务。
2. 每个流程持续显示：`源设备 → 目标设备`、Agent、范围、选中数量与库存刷新时间。
3. Push 的源固定为本机当前 Hub Snapshot；不得把 peer Shell 视图描述为远端源。
4. 远端 inventory 刷新失败时可以保留旧列表供查看，但必须标 stale、清除现有 plan 并禁用 Apply，直到成功刷新并重新 preview。
5. Apply 后逐项显示成功、失败或未知；部分失败只允许重试失败项。
6. 固定 LAN 无身份鉴权的风险披露继续有效，不使用“已认证 / 可信设备”措辞。

## 9. 异步与错误状态合同

所有 Agent Hub 请求都绑定 context fingerprint、独立 operation sequence 与 request/input fingerprint。上下文、输入、Dialog 会话或库存 snapshot 变化时，相应操作代次失效；content、preview、apply/inventory 不共享一个会互相卡住 loading 的全局 sequence。旧请求的成功、失败与 finally 都不能修改新上下文的 data、error 或 busy。

忙碌状态按操作 token 管理，不允许切换上下文时直接清全局 busy，也不允许旧操作在 finally 中解除新操作的 busy。

错误展示顺序：

1. 阻断安全错误：alert / Dialog；
2. 有旧数据的刷新错误：stale banner；
3. 单项错误：贴近资产或目标；
4. 原始 code：技术详情。

## 10. 键盘、读屏与视觉层级

1. 主 Tab 实现 roving tabindex、Left/Right、Home/End、`aria-controls` 与关联 `tabpanel`。
2. Agent 选择若表现为互斥筛选，使用 radiogroup/单选分段合同，不伪装为 Tab。
3. 冲突和放弃草稿使用共享 Dialog；阻断冲突使用 alertdialog 语义、初始聚焦安全动作、背景 inert、Escape 与焦点恢复。
4. 所有文本编辑器有准确包含 lane / Agent 的 accessible name 和 2px `:focus-visible` 轮廓。
5. 术语统一为“Agent 指令”“命令”“本机原始文件”“Hub 草稿”“技术详情”。
6. 只保留一个页面标题；刷新按钮必须说明范围；时间显示相对时间并提供完整时间 accessible label。

## 11. 迁移与兼容

1. 旧 URL 在入口规范化，不保留双轨正文渲染。
2. legacy matrix 代码可在本次删除或仅保留测试迁移所需的非生产 helper；生产 DOM 不再可达。
3. 后端 cross-agent apply command 为兼容调用方暂时保留，但固定返回稳定 blocked code；不得执行 writer。未来删除须走版本化弃用。
4. support manifest 中所有依赖未认证真实写盘的 capability 默认 blocked；scanner 不能用“已安装/本机直管”自行提升。检查脚本验证 capability 级 L3 `PASS|VERIFIED`、完整元数据与有效期；运行时在每个最终 writer 前重新 force-inspect，当前 scan-only 合同不依赖未认证 evidence。
5. `AGENT_HUB_API_VERSION=4` 表示本次 scan-only / preview-only 写策略；新 GUI 与旧 sidecar、旧 GUI 与新 sidecar的 mutation 均因 major 不匹配而失败，纯读取仍可降级。
6. 既有 Canonical 数据、revision、snapshot 与 URL 中的 Agent/Tab 尽量保留；不会迁移或删除用户资产。

## 12. 验收标准

1. 任意未认证真实 CLI 写盘入口都不可触发 filesystem mutation；直接调用兼容 command 返回稳定 blocked 错误。
2. 脏草稿刷新不会改变保存 base；CAS 漂移不能静默覆盖并发版本。
3. Original-only 场景不会触发 Hub Save 或清除其它草稿；显式解析后显示未保存并能保存 Canonical。
4. browser history / deep-link 在脏稿时不会产生标签与正文错位。
5. 切 Agent 或修改源正文后，旧跨 Agent content/preview 响应无法落入新上下文。
6. project / peer / full-stub 不再以可完成主路径呈现。
7. portable Pull 在已有 plan 后刷新失败会立即阻止 Apply。
8. legacy matrix 不在生产 DOM；Shell 与库存不再各自维护 Agent/scope。
9. 空/畸形 mutation 响应显示未知或错误，不显示成功。
10. 主 Tab、Agent 选择、库存详情和阻断 Dialog 可完全用键盘操作，并具有正确读屏语义。
11. PRD、support manifest 文档、quality evidence 与实际 UI 能力不再互相矛盾。

## 13. 明确非目标

- 本次不补齐项目级或远端设备就地管理后端。
- 本次不实现真实 CLI 写盘认证、备份/回滚或 durable mutation ledger；在这些合同完成前保持 blocked。
- 本次不把 full adapt stub 扩展成五类生产生成器。
- 本次不新增 LAN 身份鉴权、权限 token 或后台自动镜像。
- 本次不删除用户 Canonical、原生文件或 snapshot 数据。

## 14. 发布与证据门槛

- 前端需覆盖草稿 revision、history guard、async generation、stale Pull、strict decoder、Dialog 与键盘导航。
- 后端需覆盖 cross-agent apply/full 零写入、portable action/Pull 最终门禁、projection Present/Absent 删除门禁、user-instruction writer 最终门禁、混合版本阻断与 support evidence status fail-closed。
- 现有旧 E2E evidence 若依赖已删除的 legacy DOM，必须标记 superseded；新 evidence 只能声明本次真实覆盖的 preview-only 与 blocked 行为。
- 未进行真实 CLI / 多机验证的 L3 条目继续保持 `NOT VERIFIED`。

## 15. 审查记录

- 已由独立只读 reviewer 完成产品闭环审查；按审查意见补齐 Canonical/source drift 分离、真实 CAS 成功语义、统一 history dirty guard、异步 fingerprint、库存 stale、安全默认、preview-only 诚实性、键盘/Dialog 合同与 evidence 门槛。
- 实现终检以生产代码和自动化证据为准；本 HTML 原型只用于展示信息架构与关键状态，不模拟后端持久化成功。
