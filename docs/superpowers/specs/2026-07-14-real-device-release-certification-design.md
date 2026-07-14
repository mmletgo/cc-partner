# Real-Device Release Certification 设计

- 日期：2026-07-14
- 状态：已确认
- 当前执行设备：本机 Apple Silicon Mac（`arm64`）
- 当前固定 profile：`claimMode=platform-beta`、`claimProfile=macos-aarch64-beta`
- 依赖：N1–N7 全部完成并形成候选版本

## 1. 问题

现有 L0–L2 证据不能证明打包 GUI、macOS 系统权限、区域截图、全局快捷键、updater 和 VoiceOver 在真实安装包上可用。当前只有一台 Apple Silicon Mac 可执行真机认证，因此不能把 Windows、WSL、Ubuntu、Intel Mac、双物理主机、iOS、Android 或 NVDA 推定为通过，也不应让这些缺失硬件阻断本机 macOS beta。

## 2. 当前目标

1. 从 N1–N7 集成后的同一 `subjectCommit` 构建不可变 `macos-aarch64` RC artifacts，只在本机执行对应架构的真机认证。
2. 完成 `L3-MACOS-GUI-PERMISSIONS-001` 的 Apple Silicon execution 与 `L3-MACOS-VOICEOVER-001` 的 Apple Silicon execution。
3. 用机器可读证据绑定 subject、RC run、matrix、包名/SHA、macOS build、步骤、结果和脱敏 artifacts；证据有效期固定为 90 天。
4. 只允许 `macos-aarch64-beta` 的 scoped go/no-go；未认证平台继续保持 `NOT VERIFIED`，不阻断该 beta，也不产生全平台或稳定版宣称。
5. beta 若发布，只发布 Apple Silicon macOS 资产，使用 beta tag/channel 与 GitHub `prerelease: true`，绝不生成或覆盖稳定 `latest.json`。

## 3. 当前非目标

- 不执行或认证 Windows GUI、WSL/tmux、NVDA、Ubuntu x86_64/arm64。
- 不执行 Intel Mac、双物理主机 LAN/1 GiB resume、iOS Safari、Android Chrome。
- 不宣称 stable/full release、全平台支持、双机 LAN、移动真机或跨平台无障碍已认证。
- 不用 L1 浏览器 mock、hosted runner、同机 loopback 或本机 Rosetta 替代缺失的真实设备。
- 不在本计划中修复发现的产品 bug；失败回到 owning N1–N7 plan，修复后生成新 candidate 并重跑当前两项必需 execution。
- 不自动修改防火墙、权限或系统安全设置，不上传未脱敏日志、用户名、路径、token、Prompt 或文件内容。

## 4. 当前认证矩阵

| 稳定 ID | 本轮 execution | 质量矩阵聚合 | `macos-aarch64-beta` 消费规则 |
|---|---|---|---|
| `L3-MACOS-GUI-PERMISSIONS-001` | `macos-aarch64` | 因 Intel 未执行可保持 `PARTIAL`/canonical `NOT VERIFIED` | 只要匹配架构 execution 为有效 PASS 即满足 profile |
| `L3-MACOS-VOICEOVER-001` | `macos-aarch64` | 因 Intel 未执行可保持 `PARTIAL`/canonical `NOT VERIFIED` | 依赖同一 candidate 的匹配 macOS GUI execution PASS |

Windows、WSL、Ubuntu、dual-host、iOS、Android、NVDA 与 Intel Mac execution 均不创建占位 PASS manifest；现有质量矩阵对应行保持 `NOT VERIFIED`。聚合 `NOT VERIFIED` 不应覆盖或抹掉已完成的 Apple Silicon execution，checker 必须按固定 profile 读取架构级结果。

## 5. Apple Silicon Mac 验收面

### 5.1 打包、权限与生命周期

- 安装/启动签名或明确记录未签名策略的 `macos-aarch64` DMG；记录包 SHA、macOS build、签名身份非敏感摘要与 notarization 状态。
- 验证 LAN disclosure 在 GUI 启动 sidecar 前出现，并在确认后显示实际 listener；不新增认证、配对或可信设备语义。
- Accessibility、Screen Recording、Input Monitoring、Notification 四项分别执行首次拒绝、手动授权、重新检查；拒绝不得形成永久 checking 或假成功。
- 区域截图进入剪贴板；全局快捷键冲突与恢复；托盘、GUI 退出时保留 backend 和停止 backend 两种生命周期。
- updater 使用同一 `subjectCommit` 构建的隔离 N-1 certification harness，通过本机 `127.0.0.1` 临时 metadata server 指向并验签/安装同一生产 RC 的 app.tar.gz；harness 与临时 endpoint 不得进入发布资产。该证据只证明当前 subject 的 updater path，不宣称历史二进制兼容性。

### 5.2 VoiceOver

- 在同一 packaged Apple Silicon candidate 上覆盖 LAN disclosure、语义导航组、Trending 默认首页、Workbench “继续工作”与无项目 CTA、Dialog/Drawer focus return、live region、terminal tabs、Attention 导航、Human Review diff 和 WORKFLOW diagnostics。
- 每个关键动作可由 VoiceOver 找到、理解并完成；焦点不会落入 inert 背景，关闭 modal 后恢复触发点。
- VoiceOver execution 与 GUI execution 分开记结果，但必须绑定同一 `subjectCommit`、RC run 和 `macos-aarch64` 包 SHA。

## 6. Candidate、证据与发布合同

- checker/schema、RC workflow、updater harness 和 release gate 必须在 freeze 前进入 `subjectCommit`。
- 当前 RC workflow 只构建 `macos-aarch64` production artifacts 与明确 `releasable=false` 的匹配 updater harness；不启动 Windows、Linux 或 Intel Mac build jobs。未来 profile 扩展另开后续计划。
- manifest 包含稳定 ID、40 位 `subjectCommit`、Tauri version、RC workflow run id、aggregate、一个或多个独立 execution、`artifactMatrixId`、package filename/SHA、脱敏设备类、OS build、执行/过期时间、checklist 与 artifact SHA。manifest 不内嵌自身 `evidenceCommit`。
- 证据写入受保护 docs-only evidence ref；final gate 将它解析为固定 40 位 `expectedEvidenceCommit`，release workflow 必须验证 ref 未前移、RC run provenance 与所有包 SHA。
- 任一产品字节、checker/workflow 或 RC run 变化都会使当前 candidate 的两项必需 execution 失效；延期的 `NOT VERIFIED` 行无需伪造“重测”。
- `release-claim.json` 固定为 `platform-beta` + `macos-aarch64-beta`，selected matrix 只能是 `macos-aarch64`，required IDs 只能由 checker 的固定 dependency closure 派生，调用者不能传任意 allowlist。
- beta target tag/release 必须是全新且不可覆盖；只发布 DMG、对应 updater app.tar.gz/`.sig` 与 provenance。不得发布 Windows/Linux/Intel 资产，不得触碰 stable tag/channel/`latest.json`。

## 7. Go / No-Go

`macos-aarch64-beta` 仅在以下条件全部满足时为 GO：

1. `macos-aarch64` production RC、updater artifact 与 harness provenance/SHA 完整且仍可下载。
2. Apple Silicon GUI/permissions execution 为 PASS。
3. Apple Silicon VoiceOver execution 为 PASS，并绑定同一 GUI candidate。
4. manifest 未过期、已脱敏、artifact 非空，subject/tag/RC/evidence SHA 全部匹配。
5. release claim 明确列出 Windows、WSL、Ubuntu、Intel Mac、dual-host、mobile、NVDA 等 `uncertifiedSurfaces`。
6. beta 发布路径为 prerelease-only，稳定 metadata 未生成、未覆盖。

任一当前必需 execution FAIL、缺失或过期即为 NO-GO；不得以“其他平台本来就延期”为理由忽略本机失败。反之，延期平台保持 `NOT VERIFIED` 不阻断该固定 macOS beta profile。

## 8. 延期认证 backlog（不属于当前完成条件）

1. `macos-x86_64-beta`：真实 Intel Mac GUI/permissions + VoiceOver，独立包 SHA 与 execution。
2. Windows：NSIS/MSI GUI、本机 updater harness、native PTY；WSL/tmux 独立 row；NVDA 独立 row。
3. Ubuntu：x86_64 与 arm64 的 AppImage/deb、tmux/PTY、backend CLI 与 updater harness。
4. Dual-host：两台物理设备 mDNS/port fallback、socket 边界、1 GiB 断网/进程重启 resume 与最终 SHA。
5. Mobile：物理 iPhone/Safari 与 Android/Chrome 的竖横屏、safe-area、软键盘、弱网/重连。
6. `full`：上述所有固定矩阵完成后再设计稳定资产集合与全平台 go/no-go；不得由本次 beta 自动升级。

## 9. 测试与持久文档

- checker 必须有 self-test：拒绝错误 profile/matrix、缺失 GUI 或 VoiceOver execution、错误/过期 SHA、`releasable=false` 资产、stable metadata、任意 required-ID allowlist。
- 实机执行更新 `docs/development/evidence/**`、`quality-matrix.json`、`real-device-certification.md` 与 `release-claim.json`；README 只描述当前已认证 surface。
- `docs/development/testing.md` 说明架构级 PASS 与聚合 `NOT VERIFIED` 可并存，避免把单架构 beta 伪装成 full macOS 认证。

## 10. Spec 自审

- 当前唯一可执行 profile 与本机 `arm64` 条件一致，无需等待 Windows/Ubuntu。
- Trending 默认首页已进入 VoiceOver 路径，N4 决策与 N8 验收一致。
- 所有缺失硬件都保持诚实 `NOT VERIFIED`，不会形成假 PASS 或阻断本机 beta。
- 当前发布上限是 Apple Silicon macOS prerelease，不是稳定版或全平台发布。
