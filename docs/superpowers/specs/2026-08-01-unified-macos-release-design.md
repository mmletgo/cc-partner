# cc-partner 统一版本与 macOS 输入监控设计

## 目标

cc-partner 从 0.8.3 开始只保留一个用户可感知的产品版本。社区版、自用版和 Internal 不再作为产品、功能或发布通道名称；开发环境只作为同一产品的 Dev 构建存在。

统一后的应用身份：

| 构建用途 | 产品名称 | Bundle ID |
| --- | --- | --- |
| Release | `cc-partner` | `com.cc-partner.app` |
| Dev | `cc-partner (Dev)` | `com.cc-partner.app.dev` |

现有 `com.cc-partner.app.internal` 与 `com.cc-partner.app.internal.dev` 不再生成。升级到 0.8.3 后，macOS 用户需要为新的统一身份重新配置一次 TCC 权限；产品与文档必须明确提示这次迁移，不宣称旧授权能够继承。

## 设计原则

1. 功能不由代码签名决定。固定签名只改善权限身份在重建和升级之间的稳定性。
2. 无固定签名的 `.app` 仍可使用输入监控；macOS 未自动登记时，用户通过系统设置列表下方的 `+` 手动选择当前应用。
3. Dev 与 Release 使用不同 Bundle ID，避免同时安装时发生 LaunchServices 混淆，也避免调试代码继承正式应用的 TCC 授权。
4. 固定自签名证书、证书指纹 pin 和受保护的 CI secrets 仍是构建基础设施，不构成第二个产品版本。
5. 不使用私有 TCC API、不执行 `tccutil reset`、不在运行时重签、不自动重启应用。

## 构建与发布

默认 Tauri 配置继续作为产品和版本号的唯一来源，Release 固定使用 `cc-partner` 与 `com.cc-partner.app`。macOS 固定签名 overlay 只覆盖签名和 macOS 当前无法生成 updater artifact 的构建条件，不再覆盖产品名、Bundle ID 或 updater endpoint。

固定签名的 macOS Release 仍通过受保护的手动 workflow 构建。workflow、产物与终端输出统一使用 `cc-partner`，不再产生 `cc-partner Internal.app`。固定证书的 Common Name 和现有 GitHub Environment/secret 名可以保留，因为它们是私密构建边界；用户界面、安装包名称和发布文档不得把它们表述成独立版本。

macOS Dev 壳无论是否发现固定证书，都使用同一个名称、Bundle ID 和固定路径：

```text
~/Applications/cc-partner (Dev).app
com.cc-partner.app.dev
```

检测到固定证书时使用该证书签名并验证指纹；未检测到时使用 ad-hoc 签名。两种情况都能进入相同输入监控流程，差异只在于 ad-hoc 构建重建后可能需要重新手动授权。

## 输入监控状态机

后端不再按 Bundle ID allowlist 判断输入监控是否可用。macOS `.app` 统一调用公开的 CoreGraphics ListenEvent 与 IOHID API：

1. 任一公开 preflight 报告已授权时返回 `granted`。
2. 尚未在当前进程发起 Request 时，IOHID 的 `notDetermined` 或 macOS 26 的首次假 `denied` 都映射为 `notDetermined`。
3. 用户显式点击请求后，依次调用 CoreGraphics Request 与 IOHID Request；仍未授权时返回 `denied`。
4. `denied` 在 UI 中显示“在系统设置中添加”，打开输入监控设置页，并指导用户点击 `+` 选择当前 `cc-partner` 或 `cc-partner (Dev)`。
5. 未知系统返回值可保留为 `unavailable`，但它不再表示“没有固定签名”；UI 同样提供打开系统设置和手动添加路径，不显示 Internal 构建说明。

现有 DTO 的 `granted | denied | notDetermined | unavailable` 四态保持不变，不增加 wire 格式。`buildHelp` 展示动作及 `internalBuildHelp` 文案删除，避免继续制造版本概念。

## 前端体验

Welcome 与 Settings 继续复用 `usePermissions` 和 `mapPermissions`：

- `notDetermined`：显示“请求授权”。
- `denied`：显示“在系统设置中添加”。
- `unavailable`：显示“打开系统设置”，说明无法自动确认状态，但仍可手动添加。
- `granted`：无动作。

输入监控说明统一使用当前应用名称：正式版为 `cc-partner`，开发版为 `cc-partner (Dev)`。不再出现“社区版”“自用版”“内部版”“Internal app”或“没有固定签名所以不可用”等表述。

## 迁移与文档

0.8.3 是身份统一迁移版本。README、PRD、根与分层开发指令、macOS 签名手册、真实设备认证说明和质量矩阵同步更新：

- 解释一次性重新授权原因：Bundle ID 从旧 internal 身份迁移到 canonical 身份。
- 解释固定签名与 ad-hoc 的差异是授权稳定性，不是功能可用性。
- 公开 Release 在没有 Apple Developer ID 时仍不发布未公证的 macOS 包；这属于分发限制，不代表存在社区版或自用版。
- 历史设计文档保持原样，作为当时决策记录，不批量重写。

版本号通过 `scripts/bump-version.mjs 0.8.3` 同步到 Tauri 配置、Cargo package/lock 和 web package/lock，前端继续通过后端 `get_version` 读取，不硬编码版本号。

## 错误处理与安全边界

- 固定签名配置存在但证书缺失、指纹不符或签名合同失败时继续 fail closed，不回退 ad-hoc。
- 没有配置固定签名时允许明确进入 ad-hoc 模式，并打印权限可能需要重建后重新添加的提示。
- 权限查询失败不得假报 granted；未知状态保留 `unavailable`。
- 打开系统设置与 Request 仍是两条独立用户动作，不能在一次点击中同时执行。
- 固定签名 Dev 与 Release 仍使用不同 Bundle ID，权限分别授权。

## 验证

自动验证至少覆盖：

1. Rust 输入监控纯状态机：不再存在 internal Bundle ID allowlist；无签名构建状态也能走 `notDetermined -> request -> denied/granted`。
2. 前端权限映射：`denied` 与 `unavailable` 都提供手动设置路径；不存在 `buildHelp` 动作和 Internal 文案。
3. macOS Dev 壳：固定签名与 ad-hoc 解析到相同名称、Bundle ID 和固定 Applications 路径，仅签名元数据不同。
4. macOS Release overlay：产品名和 Bundle ID 继承 canonical 配置，不存在 Internal updater endpoint。
5. 签名合同：固定签名 Release 校验 `com.cc-partner.app`，Dev 校验 `com.cc-partner.app.dev`。
6. 版本合同：所有权威版本文件均为 `0.8.3`。
7. 前端 locale parity、权限单测、i18n 检查与生产构建。
8. Rust permissions 单测、格式检查和相关编译检查。

真机验证保留为 L3：分别验证固定签名与 ad-hoc `.app` 的手动 `+` 添加、授权后键鼠采样、显式重新打开，以及固定签名升级保权。自动测试不得替代真实 macOS TCC 证据。
