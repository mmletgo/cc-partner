# macOS 签名与输入监控

cc-partner 从 0.8.3 起只有一个产品版本。正式版固定使用 `cc-partner` / `com.cc-partner.app`，开发版固定使用 `cc-partner (Dev)` / `com.cc-partner.app.dev`。固定签名与 ad-hoc 签名不产生不同功能版本：两者都能使用输入监控，区别是 macOS TCC 同时绑定 Bundle ID 与代码签名 designated requirement，ad-hoc 构建重新编译后可能需要再次手动授权。

## 固定身份

- 正式版 Bundle ID：`com.cc-partner.app`
- 开发版 Bundle ID：`com.cc-partner.app.dev`
- 现有固定证书 Common Name：`cc-partner Internal Code Signing`
- 私钥只保存在构建机 Keychain、加密备份与 GitHub `internal-macos` Environment，绝不提交仓库。

证书 Common Name、Environment、环境变量和部分脚本文件名中的 `Internal` 是保留的历史构建基础设施标识，不代表独立产品版本。无需 Apple Developer 账号即可创建固定自签名证书：在“钥匙串访问 → 证书助理 → 创建证书”中使用上述名称，身份类型选“自签名根证书”，证书类型选“代码签名”，并设置足够长但有明确轮换日期的有效期。确认 identity 只出现一次：

```bash
security find-identity -v -p codesigning
security find-certificate -c 'cc-partner Internal Code Signing' -p \
  | openssl x509 -noout -fingerprint -sha256
```

把第二条命令输出的 SHA-256 指纹作为唯一预期值。构建机通过“钥匙串访问 → 导出”备份包含私钥的加密 `.p12`；使用端只导入 `.cer` 公钥证书并明确设为信任，绝不分发私钥。自签名证书只提供稳定代码身份，**不等于 Apple 公证，也不会让外部下载自动通过 Gatekeeper**。证书到期、丢失或泄露时必须创建新身份、更新指纹，并把升级视为一次 TCC 主体迁移。

## 本机固定签名构建

先安装并信任证书，再设置：

```bash
export CC_PARTNER_INTERNAL_SIGNING_IDENTITY='cc-partner Internal Code Signing'
export CC_PARTNER_INTERNAL_CERT_SHA256='<证书 SHA-256 指纹>'
scripts/build-macos-internal.sh
```

环境变量与脚本路径保留历史名称以兼容现有构建机配置。脚本生成统一的 `cc-partner.app`，并检查 canonical Bundle ID、leaf certificate SHA-256、nested code 和 designated requirement；任一步失败都会终止，不回退 ad-hoc。

签名合同单测不接触 Keychain/TCC：

```bash
node --test scripts/detect-macos-internal-signing.test.mjs scripts/check-macos-signing-contract.test.mjs scripts/prepare-macos-dev-app.test.mjs
```

## 开发壳

`./start.sh dev` 始终组装并通过 LaunchServices 启动：

```text
~/Applications/cc-partner (Dev).app
com.cc-partner.app.dev
```

启动脚本在 Keychain 中检测到唯一固定 identity 时使用固定签名。首次发现会把非敏感 SHA-256 指纹写入 `~/Library/Application Support/cc-partner/signing/internal-cert.sha256`；后续同名证书与 pin 不一致时 fail closed，避免无感切换 TCC 主体。显式设置两个环境变量具有最高优先级且必须成对提供。

未检测到固定 identity 时，同一开发壳使用 ad-hoc 签名。输入监控仍可使用：如果公开 Request 没有自动登记应用，在系统设置的输入监控列表下方点击 `+`，选择 `~/Applications/cc-partner (Dev).app` 并打开开关。重新构建后 designated requirement 可能变化，需要再次添加或重新授权。

## GitHub 固定签名构建

`.github/workflows/internal-macos.yml` 只允许手动触发，并绑定受保护 Environment `internal-macos`。workflow 与 Environment 名保留历史基础设施命名，但产物是唯一正式产品 `cc-partner`。现有四个 secret 保持不变：

- `MACOS_INTERNAL_CERTIFICATE_P12_BASE64`
- `MACOS_INTERNAL_CERTIFICATE_PASSWORD`
- `MACOS_INTERNAL_KEYCHAIN_PASSWORD`
- `MACOS_INTERNAL_CERT_SHA256`

公开 `release-tauri.yml` 在没有 Apple Developer ID 时不发布未公证的 macOS 包；Windows/Linux 与公开源码仍正常发布。这是分发限制，不是社区版/自用版差异。

固定签名 overlay 关闭 macOS updater artifact 生成，但不再覆盖产品名、Bundle ID 或 updater endpoint；这些字段继承统一正式版。当前 macOS 固定签名 artifact 采用手动覆盖安装。将来启用 macOS 自动更新时，必须同时配置 updater minisign key、公开 feed 的 macOS 条目、固定证书签名合同检查与一次升级保权 L3。

## 0.8.3 身份迁移与手动授权

旧 `com.cc-partner.app.internal` / `com.cc-partner.app.internal.dev` 的授权不会继承到 canonical Bundle ID。首次迁移到 0.8.3：

1. 启动 `cc-partner.app` 或 `~/Applications/cc-partner (Dev).app`。
2. 输入监控为 `notDetermined` 时点“请求授权”，应用在 Tauri 主线程依次调用公开 CoreGraphics ListenEvent Request 与 IOHID Request。
3. 若系统中转框未出现、列表为空、状态为 `denied` 或应用显示无法确认状态，点“在系统设置中添加”。在输入监控列表下方点 `+`，选择当前 `cc-partner` 应用，再打开开关。
4. 回到应用重新检查；若当前进程状态滞后，显式点“重新打开应用”。
5. 固定签名构建升级到下一版本时确认条目和开关保持；ad-hoc 构建重建后允许再次手动添加。

禁止使用私有 TCC API、产品内 `tccutil reset`、持久 pending marker、运行时重签或自动重启来绕过该流程。

## 真机证据

固定签名 L3 沿用稳定编号 `L3-MACOS-INPUT-MONITORING-INTERNAL-001`，编号中的 `INTERNAL` 只用于历史追踪。证据需记录完整 commit、应用版本、macOS build、`.app` SHA-256、Bundle ID、证书指纹前 12 位、designated requirement 摘要，以及脱敏的系统设置截图；不得保存 `.p12`、私钥或完整用户路径。

另需手动验证 ad-hoc `.app` 能通过列表下方 `+` 添加、授权后读取键鼠活动，并如实记录重建后是否需要重新授权。真实 `.app`、deny → grant → reopen → upgrade 流程未执行前必须保持 `NOT VERIFIED`；自动单测和 CI 不能替代该证据。
