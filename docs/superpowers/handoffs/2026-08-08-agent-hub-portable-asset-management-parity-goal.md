/goal 在独立 integration worktree 中，按唯一白名单完成 Agent Hub 三 Agent、四类资产的功能等价恢复：真实库存、本机管理、同类 Agent 远端选择性 Pull、前端闭环与证据收尾。最终交付 clean、可审查且未合并 master 的集成分支。

唯一 Plan 白名单（按顺序，禁止扫描 plans 目录、自动发现或执行其他历史/近似 Plan）：

1. `docs/superpowers/plans/2026-08-07-agent-hub-portable-asset-backend-parity.md`（8 Tasks）
2. `docs/superpowers/plans/2026-08-07-agent-hub-portable-asset-ui-parity.md`（6 Tasks）

开工前完整读取两份 Plan、根 `AGENTS.md`、目标目录逐层指令，以及权威设计 `docs/superpowers/specs/2026-08-07-agent-hub-portable-asset-management-parity-design.md`。校验 Plan 文件存在、Task 数依次为 8/6、各自恰有一个 `Task Dependency Graph`；不符即阻塞。优先级：用户本 Goal 中的确认 > 权威设计 > Plan > 分层工程指令；旧 Gate B/C/D 设计不得覆盖本次已批准决策。

建立长期 integration branch/worktree 和 durable ledger，记录 14 个 Task 的状态、基线、提交、验证、审查与 blocker。Plan DAG 固定为 Backend→UI，不允许 Plan 级并行；每份 Plan 内严格按其依赖图生成 waves，最多并行 4 个 dependency-ready Task。写任务必须使用各自 branch/worktree；共享文件、迁移、接口或测试资源冲突时收窄并行或串行，禁止扩大依赖图。集成顺序按 Task 编号；后继只从已验证的最新 Plan HEAD 开始。

每个 Task 的固定循环：从 ledger 基线建立 worktree；给 fresh implementer 提供单 Task brief、Global Constraints、允许文件和验收标准；先写/运行失败测试或失败证据，再最小实现；运行聚焦验证；自审 diff、邻接竞态与凭据泄露面；提交并生成 report（改动、测试、精确命令/结果、未验证项）；主控核对 Plan 合规后合入 Plan branch。Task 级禁止外部 reviewer，以免用局部 diff 误判跨任务合同；发现问题由 implementer 自修并重新验证。冲突解决后必须重跑该 Task 验证，不能跳审或吞失败。

每份 Plan 全部 Task 集成后，记录 clean `PLAN_BASE...HEAD`，生成包含设计、Plan、ledger、reports、diff 与验证输出的 review package；分别派 fresh Superpowers spec-compliance reviewer 和 code-quality reviewer。修复全部 High/Medium，主动扫相邻调用者、并发/重试/取消、权限/所有权、DTO/路由/i18n/文档证据面，重跑既有发现、Completion Contract 与 `Adjacent-Race Checklist`，再由两名 fresh reviewer 复审；两类均通过才允许进入下一 Plan。Low 记录 ledger，不得隐瞒。

全程序完成后，对 clean `PROGRAM_BASE...integration HEAD` 做独立终审。第 1–3 轮只通过已安装的 codex-plugin-cc：review worker 先执行 `/codex:setup`，再用插件 `/codex:*` 对 branch scope 分别做 spec/adversarial 与 code-quality 审查；禁止直接运行 `codex` CLI、插件内部脚本或 Bash 绕过。插件不可用时委派具备该插件的 worker；仍不可用则记录 blocker，不得降级冒充通过。每轮修复全部 High/Medium、补测试并重跑全套门禁；第 4 轮起如仍有问题，改用 fresh Superpowers reviewers，直至无未解决 High/Medium。

不可变业务合同：Skill/Command/Plugin/MCP 四类等权；Claude/Codex/OpenCode 均以 observed inventory 为实际状态真源；用户级和已 opt-in 项目可写，未 opt-in 项目只读/canonical-only，不猜路径；未纳管动作不得暗中 adopt；所有 mutation 走 owner 的 preview→apply→rescan 与幂等 ledger；Pull 仅 Claude→Claude、Codex→Codex、OpenCode→OpenCode，跨 target 在传输前失败；Pull 恢复远端库存、筛选、选择、冲突预览、续传、逐项报告；Plugin 删除保留 ownership 语义；MCP 凭据保持原字节且不进入 DTO、日志、错误或 DOM；LAN 不得宣称已认证；旧前端仅在 parity E2E 通过后删除，Rust/P2P N/N+1 facade 保留。

完成必须满足两份 Plan 的 Completion Contract，并提供 fresh evidence：Rust fmt/clippy/unit、两个 L2 smoke、P2P route/quality/docs；前端 unit/E2E/i18n/lint/build/bundle；14/14 Task 均有独立提交/report，Plan 与终审无 High/Medium，integration worktree clean。L1 mock 不替代 L2；未真实执行的多主机、全平台、产品版本 L3 明确保持 `NOT VERIFIED`。禁止为过测放宽断言、测试后门、静默吞错、破坏性 git、sudo 或泄露凭据。

若阻塞，停在最近 clean integrated commit，记录失败命令、完整错误、已完成/剩余 Task、尝试与安全恢复点；不得宣称完成。最终报告 integration branch、commit ranges、ledger 摘要、精确验证结果、review 引用、Low/未验证项与 blocker。未经用户额外授权，不 push、不建 PR、不合并 master、不删除用户改动；最后提供继续集成或保留分支的明确选项。
