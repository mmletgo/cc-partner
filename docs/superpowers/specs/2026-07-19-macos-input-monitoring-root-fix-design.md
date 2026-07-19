# macOS 输入监控权限根治设计

## 1. 背景与根因

cc-partner 当前的 macOS 发布包与开发壳均使用 ad-hoc 签名。其 Designated Requirement（DR）退化为具体二进制的 `cdhash`，每次重新构建都会成为新的 TCC 权限主体。现有实现又在运行时混合执行 ListenEvent Request、打开系统设置、`tccutil reset`、重签名和重启，导致“系统设置已打开但列表没有 cc-partner”、权限假绿、授权跨版本丢失和不可重复验证。

输入监控的真实消费者是 `device_query` 底层的 IOHIDManager。初版设计据此把公开 IOHID API 作为唯一权威；但 macOS 26.5.1 真机证明 `IOHIDCheckAccess` 在刚 reset、尚未请求时也会返回 Denied，`IOHIDRequestAccess` 还可能不弹中转框且不登记列表。修正版仍禁止私有 TCC API，以公开 IOHID 查询为基础，并仅用同属 ListenEvent 服务的公开 CoreGraphics preflight/request 修正该系统回归；任何返回值都不声称“列表必然已登记”，列表仍由 L3 真机确认。

## 2. 目标

1. 内部 macOS 稳定版和开发版在多次构建、升级后保持稳定 TCC 代码身份。
2. 首次点击授权时，由已签名 `.app` 的主线程触发公开 ListenEvent Request。
3. “请求授权”“打开系统设置”“重新打开应用”成为三个互不混合的显式动作。
4. 运行时不再清理用户 TCC 决策、不再重签自身、不再调用私有 TCC API。
5. GitHub 保持源码开放；无 Apple Developer ID 时，不把公开 macOS 二进制声明为可稳定继承权限的正式发行包。
6. 自动测试证明状态机、IPC、签名合同和前端行为；真实系统列表与弹窗由当前构建的 L3 真机证据证明。

## 3. 非目标

- 不绕过 Gatekeeper，不伪装 Apple 公证。
- 不自动安装或信任证书；内部设备的证书信任是一次性人工运维步骤。
- 不保证外部贡献者的 ad-hoc/本地自签名构建继承内部版权限。
- 不为旧 ad-hoc CDHash 保留授权兼容；迁移到稳定身份时允许一次重新授权。
- 不在本次更改其它 macOS 权限的产品语义，仅拆除它们与“打开设置”的混合副作用。

## 4. 稳定代码身份

### 4.1 Bundle 身份

- 内部稳定版：`com.cc-partner.app.internal`
- 内部开发版：`com.cc-partner.app.internal.dev`
- 公开源码默认构建：保留社区/无保证通道，不得被运行时识别为受支持的内部权限主体。

两个内部 Bundle ID 使用同一张长期自签名 Code Signing 证书，但拥有独立 TCC 记录，避免开发构建污染内部稳定版。

### 4.2 证书保管

- 私钥仅存在于指定签名机或受保护 CI Keychain。
- 内部使用设备只安装并信任公钥证书，不接触私钥。
- 证书 CN、SHA-256 指纹和到期时间进入内部签名运维文档与构建验证输入；证书文件和私钥不得进入仓库。
- 私钥泄露或证书轮换视为新的代码身份，必须重新授权 TCC；不得通过修改 DR 或复制 TCC 数据规避。

### 4.3 签名合同

内部构建必须满足：

1. GUI、sidecar、CLI、辅助可执行文件和最终 `.app` 均由预期证书签名。
2. `codesign --verify --deep --strict` 成功。
3. `.app` 的 identifier 与构建通道匹配。
4. DR 不得是仅绑定具体 `cdhash` 的 ad-hoc requirement。
5. 签名证书指纹与配置的预期指纹一致。

任何条件失败都阻断内部产物生成，不允许回退到 `signingIdentity: "-"`。

Tauri updater 的 minisign 签名继续用于更新包真实性；它不能替代 macOS Code Signing。更新包内 `.app` 必须通过同一内部签名合同。

## 5. 构建与发布通道

### 5.1 内部 macOS 通道

- 使用独立 Tauri config overlay，固定内部 Bundle ID 与 Code Signing identity。
- 本地构建脚本在打包前验证 Keychain identity，在打包后运行签名合同检查。
- 可选的手动 GitHub Actions workflow 从受保护 secrets 导入 `.p12` 到临时 Keychain；workflow 仅由受保护 environment/手动 dispatch 触发。
- 内部包可以使用现有 updater，但 updater 目标必须与公开社区包分开，禁止跨通道升级。

### 5.2 内部开发通道

- `./start.sh dev` 在 macOS 上生成 `.app` 壳并通过 LaunchServices 启动。
- 配置内部签名 identity 时使用 `com.cc-partner.app.internal.dev` 与固定证书，并把开发壳固定组装到 `~/Applications/cc-partner Internal (Dev).app`，供系统设置「+」选择。
- 未配置内部证书时允许普通社区开发构建，但权限页明确显示 `unavailable`，不得尝试登记输入监控。
- 开发脚本不得 ad-hoc 重签一个已由固定证书签名的 `.app`。

### 5.3 GitHub 公开通道

- 源码、Windows/Linux 产物和构建说明继续公开。
- 无 Developer ID 与公证期间，稳定公开 workflow 不发布被描述为“正式支持输入监控”的 macOS 二进制。
- 如保留社区 macOS 构建，必须使用独立名称/通道并明确标记“未公证、权限身份不保证、需自行签名”，且不得进入内部 updater feed。

## 6. 权限领域模型

### 6.1 输入监控状态

```text
InputMonitoringState = granted | denied | notDetermined | unavailable
```

- `IOHIDCheckAccess(ListenEvent) == Granted` 或 `CGPreflightListenEventAccess() == true` → `granted`
- `IOHID Denied` 且当前进程尚未显式 Request → `notDetermined`（macOS 26 假 Denied 兼容）
- `IOHID Denied` 且当前进程已经显式 Request、CG 仍未授权 → `denied`
- `Unknown` → `notDetermined`
- 不在受支持的内部 `.app` Bundle 身份中 → `unavailable`

进程内仅记录“本次已 Request”布尔态，不写磁盘、不跨重启。私有 `TCCAccessPreflight` 继续从生产代码删除。

### 6.2 显式操作

现有 `request_permission(type, openSettings?)` 被拆分：

- `request_permission(type)`：只执行该权限的公开请求 API，不打开设置。
- `open_permission_settings(type)`：只打开系统设置，不调用 Request。
- `relaunch_for_permissions()`：只在用户显式点击时重新打开 enclosing `.app`。

输入监控 Request 必须在 Tauri 主线程依次调用 `CGRequestListenEventAccess()` 与 `IOHIDRequestAccess(ListenEvent)`。两条 API 都是最佳努力，返回值不得解释为系统设置列表已经登记，并返回：

```text
PermissionActionResult {
  permission,
  operation: request | openSettings | noop,
  before,
  after
}
```

返回值只表达前后状态，不声称“系统设置列表已登记”。

### 6.3 启动合同

- 启动仅查询权限，不触发 Request、设置跳转、重置、重签或自动重启。
- 删除并忽略旧 `input-monitoring-pending-request` / `input-monitoring-cs-rotated` 标记；清理仅限删除应用自有标记文件，不调用 `tccutil`。
- 裸二进制、错误 Bundle ID 或社区构建返回 `unavailable`，给出稳定错误码 `permission_subject_unavailable`。

## 7. Welcome 与设置页交互

| 状态 | 主按钮 | 行为 |
| --- | --- | --- |
| `notDetermined` | 请求授权 | 只调用 Request |
| `denied` | 在系统设置中添加 | 只打开 Privacy_ListenEvent；文案指引点列表下方「+」选择当前 `.app` |
| `granted` | 已授权 | 无副作用 |
| `unavailable` | 查看构建说明 | 解释必须从内部签名 `.app` 启动 |

补充规则：

- Request 返回 `denied` 后，按钮切为“在系统设置中添加”；系统未自动登记时，用户点列表下方「+」选择当前应用后再开开关。
- 从系统设置返回只刷新状态；不自动 Request、不自动重启。
- 用户确认已打开开关但状态仍为 `denied` 时，才显示“重新打开应用”。
- 移除侧栏“一次请求所有权限”的批量副作用；入口只导航到权限页。
- PermissionCard 文案随操作变化，不再统一显示“去设置”。
- 所有 Hook 保持在 early return 前。

## 8. 错误处理与诊断

生产日志只记录：构建通道、Bundle ID、权限种类、操作、before/after 和稳定错误码。不得记录证书私钥、完整证书内容或用户路径。

稳定错误码：

- `permission_subject_unavailable`
- `permission_request_main_thread_failed`
- `permission_settings_open_failed`
- `permission_relaunch_failed`
- `permission_state_query_failed`

诊断页可展示非敏感信息：应用通道、Bundle ID、签名 identity 摘要、当前权限状态、是否从 `.app` 启动。不得在运行时修复签名或 TCC。

## 9. 测试设计

### 9.1 Rust L0

- 将 IOHID 查询/请求封装为可替换 provider。
- 纯状态映射覆盖 Granted/Denied/Unknown/unavailable。
- Request 测试证明公开调用顺序固定为 CoreGraphics ListenEvent 在前、IOHID 在后，且不调用设置、reset、codesign 或 relaunch。
- Open Settings 测试证明不调用 Request。
- 启动清理测试只删除应用自有 legacy marker。
- 所有测试使用隔离 `CC_PARTNER_DATA_DIR`，不得写真实 `~/.cc-partner` 或修改宿主 TCC。

### 9.2 前端 L0/L1

- 四种状态对应正确按钮与文案。
- 点击 Request 校验 permission type 与 IPC 命令。
- `denied` 只打开设置；`notDetermined` 只 Request。
- `unavailable` 不产生权限副作用。
- 返回前台只 refresh；手动 reopen 是唯一 relaunch 路径。
- E2E mock 明确声明不证明 macOS 系统列表。

### 9.3 构建合同

- 签名检查脚本带自测试 fixture：拒绝 ad-hoc、错误 Bundle ID、错误证书指纹和未签 nested binary。
- 内部 workflow 在上传 artifact 前打印非敏感签名摘要并执行严格验证。

### 9.4 L3 真机验收

必须基于当前 commit 生成的内部签名包，在 macOS 26.5.1 或更新版本执行：

1. 记录包 SHA、commit、Bundle ID、证书指纹摘要和 DR。
2. 清理仅测试 Bundle ID 的旧 TCC 状态（由人工测试准备步骤执行，不在应用内）。
3. 首次启动显示 `notDetermined`。
4. 点击“请求授权”；若系统提示出现则完成选择，若没有提示或列表仍为空，则点“在系统设置中添加”，再点列表下方「+」选择当前内部 `.app`。两条路径最终都必须让列表包含对应应用。
5. 未授权时状态为 `denied`；“在系统设置中添加”只打开设置，不触发第二个请求。
6. 打开开关并按系统要求重启后状态为 `granted`，真实 IOHID 采样可用。
7. 安装下一内部版本后 DR 兼容，权限仍为 `granted`，不重复提示。
8. Dev Bundle 的授权与内部稳定版互不影响。

未执行这组 L3 时，质量矩阵保持 `NOT VERIFIED`，不得引用冻结的 0.7.0 证据替代。

## 10. 迁移与回滚

### 10.1 首次迁移

- 内部版使用新 Bundle ID，因此不会继承旧 ad-hoc TCC 记录。
- 首次启动按 `notDetermined → request` 流程请求一次；macOS 未自动登记时按正式手动「+」兜底，不重复 Request。
- 旧版本创建的 pending/rotation marker 在应用数据目录中安全删除。
- 不自动删除旧应用、不自动重置旧 Bundle ID 的 TCC；清理由用户按文档完成。

### 10.2 回滚

- 代码可回滚到上一内部签名版本，前提是使用同一证书和 Bundle ID，TCC 身份不变。
- 不允许回滚到 ad-hoc 签名并继续复用内部 updater feed。
- 证书丢失时停止发布，创建新身份并明确要求重新授权；不得静默生成替代证书。

## 11. 完成标准

1. 生产权限路径不存在 `TCCAccessPreflight`、`tccutil`、运行时 `codesign` 或 pending 自动 Request。
2. 输入监控状态只由公开 IOHID/CG ListenEvent 预检与受支持 Bundle 身份决定；不使用私有 TCC。
3. Request/Open Settings/Reopen 三条命令和前端动作完全分离。
4. 内部构建对错误签名 fail closed，公共 workflow 不把 ad-hoc macOS 包标为正式支持。
5. Rust、前端和签名合同测试通过且不污染宿主 TCC/真实数据目录。
6. 当前内部签名构建完成 L3 deny→grant→upgrade 验收后，才能宣称根治完成。
