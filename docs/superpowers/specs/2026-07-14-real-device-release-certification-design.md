# Real-Device Release Certification 设计

- 日期：2026-07-14
- 状态：已确认
- 依赖：N1–N7 全部完成并形成候选版本

## 1. 问题

现有 L0–L2 证据强，但 `docs/development/real-device-certification.md` 中五组 L3 行仍为 `NOT VERIFIED`。自动 mock、hosted runner 和单机 loopback 不能证明打包 GUI、系统权限、Windows native/WSL+tmux、Ubuntu 安装包、两物理主机 mDNS、移动 Safari/Chrome、无障碍和 1 GiB 断点续传。

## 2. 目标

1. 在真实硬件和同一批不可变 RC artifacts 上执行五组现有 L3 认证，并拆分执行 iOS、Android、VoiceOver、NVDA 四组附加认证。
2. 证据区分二进制源码 `subjectCommit` 与承载证据 ref 解析出的 `evidenceCommit`，并包含版本、RC workflow run/artifact SHA、构建 matrix id、设备/OS、步骤、结果、脱敏截图/日志和执行时间。manifest 不内嵌自身 `evidenceCommit`，避免 Git commit 自引用；final gate 解析并冻结 40 位 `expectedEvidenceCommit`，release workflow 要求 `resolve(evidenceRef)` 与其相等，后续只用该 SHA，防止 ref 在 gate 与发布之间前移。
3. 认证证据 90 天有效；影响该能力的代码变化可提前使对应行失效。
4. release go/no-go 由机器可读矩阵和证据共同决定，失败或未执行不得写 PASS。
5. 补充 VoiceOver/NVDA、移动软键盘/safe-area 和弱网恢复场景。

## 3. 非目标

- 不用 L1 浏览器 mock 或 L2 smoke 替代真实设备。
- 不在本计划中修复发现的产品 bug；失败回到 owning N1–N7 plan 创建最小修复并重新认证。
- 不自动修改防火墙、权限或系统安全设置。
- 不上传未脱敏日志、用户名、路径、token、Prompt 或文件内容。

## 4. 认证矩阵

### `L3-MACOS-GUI-PERMISSIONS-001`

- manifest 的独立 execution 覆盖 `macos-aarch64` 与 `macos-x86_64`；若缺少真实 Intel/M 系列设备，对应 build matrix 不得进入 full 资产集合。记录 RC 的签名/未签名策略、Team ID/签名身份的非敏感摘要与 notarization 状态；启动、托盘、退出/保留 backend。
- Accessibility、Screen Recording、Input Monitoring、Notification 权限的首次拒绝、授权、重新检查。
- 区域截图进入剪贴板；全局快捷键；LAN 风险确认与实际 listener。
- updater 认证使用从同一 `subjectCommit` 构建的隔离 N-1 certification harness：应用代码与候选一致，仅版本较低，并通过专用 Tauri merge config 指向 tester 本机 `127.0.0.1` 临时 metadata server；harness 与临时配置绝不发布。metadata 指向并验签/安装同一批生产 RC 字节，完成检查、下载、验签、安装、重启和版本确认；稳定 endpoint/channel 与生产 RC 配置完全不改。该证据只证明当前 subject 的 updater 路径，不宣称某个历史已发布二进制可升级到本候选。生产构建门禁必须证明不包含 certification endpoint/不安全传输配置。

### `L3-WINDOWS-GUI-001`

- Windows NSIS setup.exe 与 MSI 分别完成干净安装/启动/卸载或修复 smoke；截图、剪贴板、全局快捷键。使用同一隔离 N-1 certification harness 与本机临时 metadata server 完成 NSIS updater artifact 的检查、下载、验签、安装、重启和版本确认，不宣称历史已发布二进制兼容性。
- native PTY fallback；WSL/tmux 只归独立 WSL row，不混入 GUI row。
- 防火墙只读提示，不自动更改规则。

### `L3-WINDOWS-WSL-001`

- 默认 WSL 发行版内 tmux 检测/安装、路径转换、window/pane 恢复。
- GUI/Mobile 同时查看时只有一个 terminal owner。

### `L3-UBUNTU-GUI-001`

- `linux-x86_64` 与 `linux-aarch64` 均需真实设备 execution，覆盖 AppImage 与 deb 安装/启动、AppImage `.sig`，并在每个架构用隔离 N-1 harness 完成同架构生产 AppImage 的检查/验签/安装/重启；同时覆盖托盘能力差异、tmux/PTY、剪贴板/截图可用性。缺少架构证据时对应资产不得进入 full。
- backend CLI start/status/doctor/stop 与 GUI lifecycle。

### `L3-DUAL-HOST-LAN-001`

- 两台物理设备 mDNS 自动发现；实际端口占用时 +1。
- 合法 LAN peer 无凭据完成 Prompt/Workbench/files/Git/terminal/Orchestrator 读写执行。
- Host/Origin/Content-Type/非法 socket peer scope、远端 stop 与超限请求按固定边界拒绝。`X-Forwarded-For`/`Forwarded` 完全不参与授权：合法 LAN socket 携带任意 XFF 仍按 LAN 处理，public socket 伪造 loopback/LAN XFF 仍因 socket peer 拒绝。
- 1 GiB 文件传输中途断网/进程重启后 resume，最终 SHA-256 一致且无重复文件。

### 移动端与无障碍附加证据

- `L3-IOS-SAFARI-001`：390×844、844×390、软键盘、safe-area、终端全屏、滚动、重连。
- `L3-ANDROID-CHROME-001`：同等布局/弱网/重连合同。
- 移动端只验证现有 `/mobile` Workbench panels（包括 Attention/Automation）；桌面 Home/Transfer deep link 在桌面 E2E/L3 验证，不虚构 mobile Home/Transfer。
- `L3-MACOS-VOICEOVER-001` 与 `L3-WINDOWS-NVDA-001`：Dialog/Drawer、侧栏分组、空态 CTA、live region、terminal tabs、review diff。VoiceOver manifest 必须按实际执行分别绑定 macOS 架构 matrix；`full` 需要两种 macOS 架构都有 execution，相应单架构 beta 只消费匹配 execution。NVDA 绑定 `windows-x86_64`。

## 5. 弱网与故障注入

在可控网络下验证 300ms RTT、1% packet loss、10 秒断网恢复：

- mobile query timeout/取消/重试；mutation uncertain reconciliation。
- remote project offline→online 后当前 panel 刷新。
- Cloud Sync partial/unreachable 不显示成功。
- Transfer resume 与 Orchestrator owner event/Attention deep link 不重复动作；系统通知只验证显示、权限和隐私文案，不承诺当前桌面插件不支持的点击回调。

## 6. 证据格式

每条 evidence 记录：

证据 manifest schema v1 顶层必须包含 `id`、完整 40 位 `subjectCommit`、与 `tauri.conf.json` 一致的 `version`、`rcWorkflowRunId`、aggregate `PASS | FAIL | PARTIAL` status、`executions[]` 与脱敏说明。每个 execution 强制包含独立 `PASS | FAIL` status、一个或多个精确 `artifactMatrixIds`/安装包 filename+SHA、脱敏 `deviceClass`、精确 `osBuild`、RFC3339 `executedAt`、恰好晚 90 天的 `expiresAt`、检查项和相对 artifact 路径/SHA-256；PASS execution 的所需 artifact 不得为空。aggregate PASS 表示该稳定 ID 的完整 full-contract execution set 全部 PASS；任一已执行项 FAIL 则 aggregate FAIL；至少一项 PASS、无 FAIL 但仍缺 full-contract execution 时为 PARTIAL。checker 必须按 matrix id + filename 将每个 package SHA 与 RC inventory 的对应 entry 做精确相等比较，既不接受只在 evidence 目录自洽的 SHA，也不接受其它 matrix 的同名包。质量矩阵保持 canonical `PASS | FAIL | NOT VERIFIED`：aggregate PASS→PASS，aggregate FAIL→FAIL，PARTIAL→NOT VERIFIED；从未执行的 NOT VERIFIED 必须 `evidenceManifest:null`，PARTIAL 映射的 NOT VERIFIED 必须指向真实 manifest。`commit=subjectCommit`。profile gate 按匹配 matrix execution 的 status 判定，因此一个架构 PASS、另一架构未执行/FAIL 时，可发布仅依赖前者的固定 beta，但 full 与失败架构仍阻断。checker 读取并校验真实文件，不只检查 JSON 行。release workflow 从 `evidenceRef` 解析 40 位 `evidenceCommit` 并写入不可变 release provenance asset/attestation，而不是写回 evidence ref。

实际文件中必须填真实值；模板示例不进入 PASS evidence。截图和日志保存到既有 certification evidence 目录，先运行脱敏检查。

## 7. Go/No-Go

- checker/schema/workflow 变更必须在 freeze 前进入 `subjectCommit`。随后 `workflow_dispatch` RC build 从该 commit 生成不公开的 Actions artifacts；L3 evidence 在 docs-only 后继 `evidenceCommit`/受保护 evidence ref 中引用 subject/artifact SHA。最终 release 下载并发布同一 RC artifacts，tag 指向 `subjectCommit`，不重新构建。任何产品字节、checker/workflow 或 `rcWorkflowRunId` 变化都生成新 candidate，并要求九行全部重跑；本轮不实现按影响图复用旧证据。
- freeze 前必须把版本提升到高于既有发布的唯一 semver（audited 0.6.7 baseline 已存在 `v0.6.7`，默认下一未占用 minor 为 0.7.0），并通过本地 tag + GitHub Releases API 双重确认目标 `v<version>`、beta tag 与 `subjectTag` 均不存在。bump 后全门禁通过才可 freeze；release workflow 对任何已存在 tag/release fail-closed，禁止 force-move 或覆盖资产。
- machine-readable `claimMode` 仅允许 `full` 或 `platform-beta`，并要求固定 `claimProfile`、`claimedSurfaces`、`uncertifiedSurfaces` 和构建 matrix/asset 映射。`full` 表示五个 build matrix 与本 spec 列出的九组表面通过，不得扩写为所有平台的所有辅助技术；它要求五个现有 L3 与四个新增稳定 ID 均在有效期内 PASS，并覆盖 `macos-aarch64`、`macos-x86_64`、`windows-x86_64`、`linux-x86_64`、`linux-aarch64`。
- 仅平台特定 beta 可在发布说明中明确列出其它平台 `NOT VERIFIED`，不得使用全平台措辞。
- 任一 FAIL 阻断相关宣称；任何需要修改产品/checker/workflow 或生成新 RC 的修复均按同一 candidate 合同重跑九行。
- `platform-beta` 不接受任意 ID allowlist：checker 内置固定 profile→asset matrix→required L3 dependency closure，并派生 certified/uncertified IDs/surfaces。beta 只发布 profile 对应资产，使用 beta tag/channel、GitHub `prerelease: true`，绝不生成/覆盖稳定 `latest.json`；只有 `full` 可更新默认 updater metadata。
- checker 内置且只接受以下固定 profile：
  - `full`：五个 matrix、九个 L3 ID；发布 macOS 每架构的 DMG + updater app.tar.gz + `.sig`、Windows NSIS setup.exe + `.sig` + MSI、Linux 每架构 AppImage + `.sig` + deb。RPM 未有本 spec 真机安装证据，所有 profile 都排除。
  - `macos-aarch64-beta` / `macos-x86_64-beta`：只选对应 macOS matrix/上述 macOS 资产；要求该架构 `L3-MACOS-GUI-PERMISSIONS-001` execution + `L3-MACOS-VOICEOVER-001`，只宣称本机 GUI/权限/截图/快捷键、certification-harness updater path 与 VoiceOver。
  - `windows-x86_64-beta`：只选 Windows matrix/上述 Windows 资产；要求 `L3-WINDOWS-GUI-001` + `L3-WINDOWS-NVDA-001`，只宣称本机 GUI/native PTY/截图/快捷键、certification-harness updater path 与 NVDA，不宣称 WSL。
  - `windows-wsl-x86_64-beta`：同一 Windows 资产；额外要求 `L3-WINDOWS-WSL-001`，才增加 WSL/tmux surface。
  - `linux-x86_64-beta` / `linux-aarch64-beta`：只选对应 Linux matrix 的 AppImage + `.sig` + deb；要求 `L3-UBUNTU-GUI-001` 中对应架构 execution，只宣称该架构本机 GUI/tmux/PTY/backend CLI 与 certification-harness updater path，不宣称 screen reader。
  - dual-host LAN、移动浏览器和跨平台综合表面仅在 `full` 中宣称；没有独立 mobile-only 发布资产/profile。所有 beta 的其它 OS、LAN/mobile、跨设备 Orchestrator 与未执行辅助技术进入 `uncertifiedSurfaces`。
- release workflow 在 `subjectCommit` checkout 中执行受审计的 checker/workflow 代码，只把受保护 evidence ref checkout 到独立只读目录。diff allowlist 仅为 `README.md`、`docs/development/{quality-matrix.json,real-device-certification.md,release-claim.json}`、`docs/testing/mobile-workbench-lan-test-cases.md` 与 `docs/development/evidence/**` regular files；绝不执行 evidence ref 中的脚本或 workflow。它要求运行时解析的 ref 恰好等于 final gate 输入的 `expectedEvidenceCommit`，验证 `subjectCommit` 是其祖先、`subject..expectedEvidenceCommit` diff 只含 allowlist、无 submodule/symlink/path escape，再只用该 commit 进行后续步骤。
- GitHub workflow-dispatch REST API 的 `ref` 只接受 branch/tag，因此 freeze 时创建受保护、禁止移动/删除的 `subjectTag=cert-subject-v<version>-<12sha>`，并验证其 peel 后恰好等于 40 位 `subjectCommit`。RC 与 evidence-aware release 都以 `ref=subjectTag` dispatch，同时把完整 subject SHA 作为 input；首个 job 在任何 checkout/下载/发布前断言 `github.sha == inputs.subjectCommit`，并再次校验 tag 指向。因此 GitHub 解释的 workflow YAML、reusable workflow 与仓库内 checker 都来自 subject，而不是默认分支或 evidence ref；第三方 actions 继续固定到受审计 SHA。
- release gate 通过 GitHub Actions API 校验 RC run 的 repository、workflow path/id、event、`head_sha=subjectCommit`、success conclusion、未删除且 artifacts 未过期，再核对 inventory 的名称、数量和 SHA。RC artifact 显式使用平台允许的最大 retention 并记录 `artifactExpiresAt`；不可下载即使 manifest 未过期也必须新建 RC 并九行重测。
- 固定 profile dependency closure 至少满足：任何 WSL claim 依赖 Windows GUI；macOS a11y 依赖对应架构 macOS GUI；移动 browser claim 依赖一个已认证 host GUI、dual-host LAN 与对应 browser row。checker 自动展开，不能由人工 allowlist 绕过。
- `claimedSurfaces` 精确列出九行证明的能力；Ubuntu screen reader、iOS VoiceOver、Android TalkBack 等未执行表面进入 `uncertifiedSurfaces`。Windows 截图/剪贴板/快捷键由 Windows GUI row 覆盖。
- umbrella 的广义全仓门禁、事实校准与非 evidence-allowlist 文档 commit 必须在 `subjectCommit` freeze 前完成；L3 执行后只允许 claim/evidence 文件变化，并再运行不改字节的 evidence/docs/path gate。N8 先冻结 publish input bundle，evidence-aware release dispatch 是整个计划最后一个不可逆动作，之后不再修改产品/checker/workflow/证据；任何需要修改的失败都创建新 candidate 并九行重测。

## 8. 测试与验收

1. certification schema 自测拒绝占位 SHA、过期证据、缺失/哈希不符 artifact、manifest package SHA 与 RC inventory entry 不等、未知 status、subject/evidence 混淆、symlink/path escape 和 matrix/manifest 不一致。
2. docs/quality traceability 检查证据 ID 与质量矩阵一致。
3. 五组现有与四组附加真实步骤各自有独立人工结果/脱敏 artifact；未执行保持 canonical `NOT VERIFIED`，不聚合成假 PASS。
4. 发现 bug 时记录 owning plan、复现和修复 commit；修复进入新 `subjectCommit`/RC 后九行全部重测，禁止只改 manifest 的 commit/run 或写“预计通过”。
5. release note/README 保证措辞与当前有效证据一致。

## 9. 持久文档

freeze 前更新 `testing.md` 方法论与发布 workflow；真实执行后只在 evidence allowlist 内更新 `docs/development/real-device-certification.md`、`quality-matrix.json`、平台测试用例、README、release claim 与 evidence。只有真实执行后才修改 PASS 状态。

## 10. Spec 自审

- 五组现有 L3 与新增无障碍/弱网场景边界清晰。
- 90 天有效期、失效条件和 go/no-go 无歧义。
- 本计划只认证，不在证据分支静默修 bug。
