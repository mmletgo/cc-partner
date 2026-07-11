/goal 在独立 integration/plan worktree 中，严格按下列唯一白名单和顺序，用 Superpowers Subagent-Driven Development 完成 cc-partner 工程改进与全局 Inbox 共 65 个 Task；每个 Task 均采用 fresh implementer、TDD、独立提交、Codex 审查、必要修复与复审，最终形成可审查、证据完整且未合并到 master 的集成分支。

唯一执行范围（禁止扫描 plans 目录、自动发现或执行任何历史/近似 Plan）：

1. `/Users/hans/web_project/cc-partner/docs/superpowers/plans/2026-07-11-vitest-frontend-ci.md`（8）
2. `/Users/hans/web_project/cc-partner/docs/superpowers/plans/2026-07-11-workbench-controller-extraction.md`（9）
3. `/Users/hans/web_project/cc-partner/docs/superpowers/plans/2026-07-11-p2p-protocol-metadata-errors.md`（9）
4. `/Users/hans/web_project/cc-partner/docs/superpowers/plans/2026-07-11-remote-orchestrator-runtime-snapshot.md`（8）
5. `/Users/hans/web_project/cc-partner/docs/superpowers/plans/2026-07-11-global-inbox.md`（10）
6. `/Users/hans/web_project/cc-partner/docs/superpowers/plans/2026-07-11-cross-platform-smoke-ci.md`（6）
7. `/Users/hans/web_project/cc-partner/docs/superpowers/plans/2026-07-11-backend-logs-doctor.md`（8）
8. `/Users/hans/web_project/cc-partner/docs/superpowers/plans/2026-07-11-documentation-calibration.md`（7）

执行前完整读取根 `AGENTS.md`、目标目录分层指令，以及：

- `docs/superpowers/specs/2026-07-11-engineering-improvement-program-design.md`
- `docs/superpowers/specs/2026-07-11-global-inbox-design.md`

先校验 8 个文件存在、Task 数依次为 `8 9 9 8 10 6 8 7`，总数为 65；不匹配即阻塞，不得用旧 Plan 替代。读取并使用 `superpowers:using-git-worktrees`、`subagent-driven-development`、`test-driven-development`、`systematic-debugging`、`verification-before-completion`、`finishing-a-development-branch`。创建长期 integration worktree/branch `sdd/cc-partner-improvement-program`，每份 Plan 建独立 branch/worktree，从当时 integration HEAD 开始；Plan 必须串行，写入型 Agent 不并行。

为 65 个 Task 建 durable ledger。每个 Task 严格执行：主控提取单个 task brief 和 Global Constraints → 派 fresh implementer subagent → 先建立失败证据，再最小实现和聚焦测试 → implementer 自审、提交并写 report（修改、测试文件、精确命令/结果）→ 生成 BASE..HEAD review package → 调用 Codex reviewer → 修复 Critical/Important → 用同一 BASE 再次调用 Codex，直至 `Spec compliance: ✅` 且 `Code quality: Approved`。Minor 必须进入 ledger，不能静默丢弃。一个 Task clean 后才进入下一个；一份 Plan clean 后才本地合并 integration。

所有 task、Plan-level 和 whole-program reviewer 必须由 Claude Code 已安装的 `codex-plugin-cc` 调用 Codex；Claude 主控、implementer 或其他 Claude subagent 不得充当 reviewer。预检运行 `/codex:setup`；插件、Codex CLI 或认证不可用即报告 blocker，禁止降级或跳审。自定义审查统一使用：

`/codex:adversarial-review --wait --base <BASE> --scope branch <FOCUS>`

FOCUS 必须要求 Codex 读取 task brief/对应 Plan、spec、implementer report、review package、Global Constraints 和 ledger，并输出严重级别、`file:line`、证据、违反要求、修正建议及 `Cannot verify from diff`。完整保留 Codex 输出，不得改写；缺少两个 verdict 时审查未完成。若只能后台运行，记录 job ID，用 `/codex:status <id> --wait` 等待并以 `/codex:result <id>` 取回完整结果后方可继续。

每份 Plan 完成后，以 PLAN_BASE..HEAD 生成 package，再由 Codex 做 Plan-level broad review，覆盖 Completion Contract、spec、全部 Task 与 Minor；修复并复审 clean 后才合并 integration。8 份 Plan 完成后，以 PROGRAM_BASE..integration HEAD 生成 package，由 Codex 做 whole-program final review，重点验证跨阶段协议、runtime/attention 兼容、Workbench/Inbox deep-link、Retry/Discard 自动移除、跨平台 CI、日志/doctor artifacts 及 README/PRD 事实一致性。

遵守现有产品哲学：Inbox 只聚合“需要我处理什么”，主列表即时更新，不提供已解决历史；桌面为独立页面，移动端为导航第二项，不新增浮层、顶栏常驻入口或自动抢占首屏。复用现有组件/API；UI 遵守 huashu-design 已确认方向和 token 体系。保留用户改动；禁止 destructive git、sudo、泄露凭据、为过测而放宽断言或吞异常。数据库变更必须迁移/兼容/回滚，Inbox 禁止新增业务表。

完成标准：65/65 Task 均有提交、report、TDD/验证证据和 clean Codex review；8 个 Completion Contract 均通过新鲜验证；Plan-level 与 whole-program Codex review 无未解决 Critical/Important；integration 工作区干净。最终报告 branch/commit ranges、精确验证命令与结果、Codex review/job 引用、剩余 Minor、明确未验证的平台/托管行为和 blocker。未经额外授权，不 push、不创建 PR、不合并 master；最后使用 `superpowers:finishing-a-development-branch` 向用户提供集成选项。
