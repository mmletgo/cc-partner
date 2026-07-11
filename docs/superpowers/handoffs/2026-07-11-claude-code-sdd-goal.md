/goal 在独立 integration/plan worktree 中，严格按下列唯一白名单和顺序，用 Superpowers Subagent-Driven Development 完成 cc-partner 工程改进与全局 Inbox 共 65 个 Task；每个 Task 均采用 fresh implementer、TDD、独立提交、Codex 审查、必要修复与复审，最终形成可审查、证据完整且未合并到 master 的集成分支。

唯一执行范围（`PLAN_ROOT=/Users/hans/web_project/cc-partner/docs/superpowers/plans`；禁止扫描目录、自动发现或执行历史/近似 Plan）：

1. `$PLAN_ROOT/2026-07-11-vitest-frontend-ci.md`（8）
2. `$PLAN_ROOT/2026-07-11-workbench-controller-extraction.md`（9）
3. `$PLAN_ROOT/2026-07-11-p2p-protocol-metadata-errors.md`（9）
4. `$PLAN_ROOT/2026-07-11-remote-orchestrator-runtime-snapshot.md`（8）
5. `$PLAN_ROOT/2026-07-11-global-inbox.md`（10）
6. `$PLAN_ROOT/2026-07-11-cross-platform-smoke-ci.md`（6）
7. `$PLAN_ROOT/2026-07-11-backend-logs-doctor.md`（8）
8. `$PLAN_ROOT/2026-07-11-documentation-calibration.md`（7）

执行前完整读取根 `AGENTS.md`、目标目录分层指令，以及：

- `docs/superpowers/specs/2026-07-11-engineering-improvement-program-design.md`
- `docs/superpowers/specs/2026-07-11-global-inbox-design.md`

先校验 8 个文件存在、Task 数依次为 `8 9 9 8 10 6 8 7`，总计 65；不匹配即阻塞。读取并使用 `superpowers:using-git-worktrees`、`subagent-driven-development`、`dispatching-parallel-agents`、`test-driven-development`、`systematic-debugging`、`verification-before-completion`、`finishing-a-development-branch`。创建长期 integration branch/worktree `sdd/cc-partner-improvement-program`；8 个 Plan 保持上述阶段顺序，后一 Plan 只能从前一 Plan clean 并合入 integration 后开始。

为 65 个 Task 建 durable ledger。每份 Plan 的 `Task Dependency Graph` 是最大并行上界：主控按图生成 waves，同一 wave 最多并行 4 个 fresh implementer，每个 Task 使用从 wave 基线创建的独立 branch/worktree。开工前复核当前代码；发现新增依赖、共享接口/迁移、文件或测试资源冲突时只可拆 wave/串行并记录依据，不得扩大 Plan 图中的并行范围；不确定时串行。

每个 Task 执行：主控给出单个 brief/Global Constraints → implementer 先建立失败证据，再最小实现和聚焦测试 → 自审、提交并写 report（修改、测试文件、精确命令/结果）→ 生成 BASE..HEAD package → Codex review → 修复 Critical/Important → 同一 BASE 复审，直至 `Spec compliance: ✅` 且 `Code quality: Approved`。Minor 写入 ledger。主控仅合入 clean Task，并按依赖拓扑逐个合入 Plan branch；冲突 Task须基于最新 Plan HEAD 重放/修复、重跑验证并重新接受 Codex review，不得直接手工消冲突后跳审。一个 wave 全部合入且集成验证通过后，才启动依赖它的下一 wave。

主控与 implementer 可使用任意支持本 Goal 所需 subagent、worktree 和验证能力的 Agent，不绑定具体厂商或模型。所有 task、Plan-level 和 whole-program reviewer 必须通过已安装的 [**codex-plugin-cc**](https://github.com/openai/codex-plugin-cc) 调用 Codex：主控若不能直接使用插件 slash command，必须把纯 review 步骤委派给可使用该插件的独立 review worker。只能使用插件暴露的 `/codex:*`；**禁止直接执行 `codex` CLI、直接调用插件内部 script/companion runtime，或用 Bash 绕过插件**。主控、implementer 及其同源 subagent 不得自行充当 reviewer。review worker 预检运行 `/codex:setup`；插件报告 Codex CLI/认证不可用时即报告 blocker，禁止自行改用 CLI、降级或跳审。自定义审查统一使用：

`/codex:adversarial-review --wait --base <BASE> --scope branch <FOCUS>`

FOCUS 必须要求 Codex 读取 task brief/对应 Plan、spec、implementer report、review package、Global Constraints 和 ledger，并输出严重级别、`file:line`、证据、违反要求、修正建议及 `Cannot verify from diff`。完整保留 Codex 输出，不得改写；缺少两个 verdict 时审查未完成。若只能后台运行，记录 job ID，用 `/codex:status <id> --wait` 等待并以 `/codex:result <id>` 取回完整结果后方可继续。

每份 Plan 完成后，以 PLAN_BASE..HEAD 生成 package，再由 Codex 做 Plan-level broad review，覆盖 Completion Contract、spec、全部 Task 与 Minor；修复并复审 clean 后才合并 integration。8 份 Plan 完成后，以 PROGRAM_BASE..integration HEAD 生成 package，由 Codex 做 whole-program final review，重点验证跨阶段协议、runtime/attention 兼容、Workbench/Inbox deep-link、Retry/Discard 自动移除、跨平台 CI、日志/doctor artifacts 及 README/PRD 事实一致性。

遵守现有产品哲学：Inbox 只聚合“需要我处理什么”，主列表即时更新，不提供已解决历史；桌面为独立页面，移动端为导航第二项，不新增浮层、顶栏常驻入口或自动抢占首屏。复用现有组件/API；UI 遵守 huashu-design 已确认方向和 token 体系。保留用户改动；禁止 destructive git、sudo、泄露凭据、为过测而放宽断言或吞异常。数据库变更必须迁移/兼容/回滚，Inbox 禁止新增业务表。

完成标准：65/65 Task 均有提交、report、TDD/验证证据和 clean Codex review；8 个 Completion Contract 均通过新鲜验证；Plan-level 与 whole-program Codex review 无未解决 Critical/Important；integration 工作区干净。最终报告 branch/commit ranges、精确验证命令与结果、Codex review/job 引用、剩余 Minor、明确未验证的平台/托管行为和 blocker。未经额外授权，不 push、不创建 PR、不合并 master；最后使用 `superpowers:finishing-a-development-branch` 向用户提供集成选项。
