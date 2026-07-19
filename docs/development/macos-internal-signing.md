# macOS 内部签名与输入监控

cc-partner 的输入监控只支持固定内部签名通道。原因是 macOS TCC 同时绑定 Bundle ID 和代码签名 designated requirement；ad-hoc 签名的 requirement 会随构建 CDHash 漂移，应用可能无法出现在“系统设置 → 隐私与安全性 → 输入监控”列表。

## 固定身份

- 稳定版 Bundle ID：`com.cc-partner.app.internal`
- 开发版 Bundle ID：`com.cc-partner.app.internal.dev`
- 证书 Common Name：`cc-partner Internal Code Signing`
- 私钥只保存在构建机 Keychain、加密备份与 GitHub `internal-macos` Environment，绝不提交仓库。

无需 Apple Developer 账号。在“钥匙串访问 → 证书助理 → 创建证书”中：名称固定为 `cc-partner Internal Code Signing`，身份类型选“自签名根证书”，证书类型选“代码签名”，并设置足够长但有明确轮换日期的有效期。创建后确认该 identity 在以下命令中只出现一次：

```bash
security find-identity -v -p codesigning
security find-certificate -c 'cc-partner Internal Code Signing' -p \
  | openssl x509 -noout -fingerprint -sha256
```

把第二条命令输出的 SHA-256 指纹作为唯一预期值。构建机通过“钥匙串访问 → 导出”备份包含私钥的加密 `.p12`；使用端只导入 `.cer` 公钥证书到登录钥匙串并在证书“信任”中明确设为始终信任，绝不分发私钥。自签名证书只用于稳定内部代码身份，**不等于 Apple 公证，也不会让外部下载自动通过 Gatekeeper**。证书到期、丢失或泄露时必须创建新身份、更新指纹，并把升级视为一次 TCC 主体迁移，所有机器重新授权；禁止静默生成同名替代证书。

## 本机构建

先安装并信任证书，再设置：

```bash
export CC_PARTNER_INTERNAL_SIGNING_IDENTITY='cc-partner Internal Code Signing'
export CC_PARTNER_INTERNAL_CERT_SHA256='<证书 SHA-256 指纹>'
scripts/build-macos-internal.sh
```

脚本会检查 Keychain identity、Bundle ID、leaf certificate SHA-256、nested code 和 designated requirement。任一步失败都会终止，不回退 ad-hoc。

首次使用时可先只运行签名合同单测，不接触 Keychain/TCC：

```bash
node --test scripts/detect-macos-internal-signing.test.mjs scripts/check-macos-signing-contract.test.mjs scripts/prepare-macos-dev-app.test.mjs
```

内部开发机安装固定 identity 后，直接运行 `./start.sh dev` 即会自动生成 `cc-partner-internal-dev.app`。启动脚本只在 Keychain 中恰好存在一个固定名称的有效代码签名 identity 时启用内部通道；首次发现会把非敏感 SHA-256 指纹写入 `~/Library/Application Support/cc-partner/signing/internal-cert.sha256`，后续同名证书与 pin 不一致时 fail closed，避免无感切换 TCC 主体。显式设置两个环境变量仍具有最高优先级，且必须成对提供。

没有安装内部 identity 的开源贡献者仍生成社区 Dev 壳；它可以开发其它功能，但输入监控会显示 `unavailable`，不会误导用户打开空设置列表。社区模式与自动检测结果都会由 `start.sh` 明确打印，禁止检测失败后静默回退 ad-hoc。

## GitHub 内部构建

`.github/workflows/internal-macos.yml` 只允许手动触发，并绑定受保护 Environment `internal-macos`。配置四个 secret：

- `MACOS_INTERNAL_CERTIFICATE_P12_BASE64`
- `MACOS_INTERNAL_CERTIFICATE_PASSWORD`
- `MACOS_INTERNAL_KEYCHAIN_PASSWORD`
- `MACOS_INTERNAL_CERT_SHA256`

公开 `release-tauri.yml` 不发布 macOS ad-hoc 正式产物；Windows/Linux 与公开源码仍正常发布。

当前内部 overlay 关闭 updater artifact 生成，并把检查地址隔离到 `internal-macos` 专用 feed；该 feed 尚未发布，所以内部版采用手动覆盖安装。不得让内部 Bundle ID 消费公开 `latest.json`。将来启用内部自动更新时，必须同时配置独立 minisign key、专用 feed、固定证书签名后的 `.app` 合同检查与一次升级保权 L3，四项缺一不可。

## 一次迁移与真机验证

旧 `com.cc-partner.app` / `com.cc-partner.app.dev` 条目不会继承到 internal Bundle ID。首次迁移：

1. 安装并信任内部公钥证书，启动签名校验通过的 `.app`。
2. Welcome 输入监控为 `notDetermined` 时点“请求授权”，只调用一次 IOHID Request。
3. 状态变为 `denied` 后点“打开系统设置”，确认列表出现 `cc-partner Internal` 并打开开关。
4. 回到应用重新检查；若当前进程状态滞后，显式点“重新打开应用”。
5. 升级到下一内部构建，确认条目和开关保持，不重新授权。

保存证据时记录完整 commit、应用版本、macOS build、`.app` SHA-256、Bundle ID、证书指纹前 12 位、designated requirement 摘要，以及已脱敏的系统设置截图；不得保存 `.p12`、私钥或完整用户路径。

证据编号 `L3-MACOS-INPUT-MONITORING-INTERNAL-001`。在真实证书、真实 `.app`、deny→grant→reopen→upgrade 全流程未执行前必须保持 `NOT VERIFIED`；自动单测和 CI 不能替代该证据。
