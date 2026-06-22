# 截图权限主动引导设计

## 背景

当前 `./start.sh` 启动不触发 macOS 授权框,根因(详见 2026-06-22 排查):

1. 启动流程(`lib.rs` setup)不调任何权限 request API;
2. `OnboardingGuard`(`App.tsx`)只用 `check_permissions` = `CGPreflightScreenCaptureAccess`(预检,**永不弹框**);
3. 真正能弹系统框的 `request_permission`(`CGRequestScreenCaptureAccess`)只在 Welcome 页用户**手点**「去授权」时触发;
4. 截图入口 `overlay::start_region_capture` **无任何权限预检**,未授权时直接抓到空白图。

## 目标

1. **启动主动引导**:首次启动检测到截图相关权限未就绪时,主动引导(`screenCapture` 弹系统框;`inputMonitoring` 开设置面板),并进入 Welcome 页。
2. **截图入口预检**:截图快捷键/按钮触发时,若屏幕录制未授权,**不抓空白图**,而是显示主窗口 + 跳 Welcome 引导。

## 权限分工(技术事实,决定落地方式)

| 权限 | 作用 | 缺失后果 | 引导位置 |
|------|------|----------|----------|
| 屏幕录制 Screen Capture | 截图抓屏直接依赖(`xcap`) | 抓到空白图 | `start_region_capture` **阻断式预检** + 启动 onboarding |
| 输入监控 Input Monitoring | 只影响快捷键能否触发 handler | 快捷键不触发(handler 收不到调用,无法在截图入口预检) | 启动 onboarding + 常驻权限徽标(`PermissionStatusBadge`) |

> 关键:输入监控缺失时快捷键压根不触发,所以**无法在 `start_region_capture` 入口预检它**(到不了那里)。它不属于"截图能不能用",而属于"快捷键能不能用",靠 onboarding + 徽标引导。

## 改动 1:启动主动引导(OnboardingGuard)

### 后端 `permissions/mod.rs` + `commands/permissions.rs`

- `request_permission(perm_type, open_settings: Option<bool>)` 新增可选参数,默认 `true`(保持现有行为):
  - `screenCapture`:**总调** `CGRequestScreenCaptureAccess`(未决定才弹框);`open_settings=true` 时再 `open_permission_settings` 兜底。
  - `inputMonitoring`:无系统 request API;`open_settings=true` 时 `open_permission_settings`;`false` 时 no-op。
  - 返回结构 `{ok, requested, opened}` **不变**。
- `commands::permissions::request_permission(r#type, open_settings)` 透传新参数。

### 前端 `api/config.ts` + `App.tsx`

- `configApi.requestPermission(type, openSettings?: boolean)`。
- `OnboardingGuard`:检测到未全部授权时,对**每项未授权权限**按类型调 request,然后 redirect `/welcome`:
  - `screenCapture` 未授权 → `requestPermission('screenCapture', false)`(仅弹系统框)
  - `inputMonitoring` 未授权 → `requestPermission('inputMonitoring', true)`(开设置面板,它唯一的主动引导方式)

## 改动 2:截图入口预检(`screenshot/overlay.rs`)

### 复用提取(规则 9)

- `tray.rs:29 show_main_window(app)` 提升为 `pub(crate)`(主窗口 label = `"main"`),供 `tray` + `overlay` 复用,避免重复实现。

### 后端

- `start_region_capture` 开头加预检(此函数是命令层 + `hotkey::screenshot_handler` 的**唯一入口**,一处覆盖两条路径):

```rust
pub fn start_region_capture(app: &AppHandle) -> Result<(), AppError> {
    if !crate::permissions::check_screen_capture_access() {
        crate::tray::show_main_window(app);          // 显示主窗口(可能被托盘隐藏)
        let _ = app.emit("screenshot:permission-needed", ());
        return Ok(());                               // 不开 overlay,不抓空白图
    }
    // …原枚举显示器开 overlay 逻辑不变
}
```

### 前端 `App.tsx`

- 新增顶层监听组件 `PermissionNeededListener`(挂在 `<Routes>` 同级,在 `BrowserRouter` context 内):
  - `useEffect` + `listen('screenshot:permission-needed')` → `navigate('/welcome')`。
- Welcome 页本就展示两项权限卡片,跳转即同时覆盖 `inputMonitoring` 引导。

## 范围边界(YAGNI)

- 截图入口只对**屏幕录制**做阻断预检(截图直接依赖)。`inputMonitoring` 靠 onboarding + 常驻徽标,不在截图入口预检(技术上不可行)。
- 不改 `request_permission` 返回结构,只加可选入参 → 无前端 breaking。
- 不处理"注册快捷键时检测输入监控并提示"(onboarding + 徽标已足够,避免过度工程)。

## 验证(规则 11/12,亲自跑)

1. 清 `localStorage cp-permission-onboarded` → `./start.sh` 启动 → 首次应:屏幕录制弹系统框 + 输入监控开设置面板 + 进入 Welcome。
2. 未授权状态下按截图快捷键 → 主窗口显示 + 跳 Welcome(**非**空白图)。
3. 授权后按截图快捷键 → 正常选区 overlay。
4. 更新 `src-tauri/CLAUDE.md`(M6/M7 节)与 `web/CLAUDE.md`(macOS 权限流程节)记录新行为(规则 5)。

## 涉及文件清单

**后端**:
- `src-tauri/src/permissions/mod.rs`(`request_permission` 加 `open_settings` 参数)
- `src-tauri/src/commands/permissions.rs`(透传参数)
- `src-tauri/src/screenshot/overlay.rs`(入口预检)
- `src-tauri/src/tray.rs`(`show_main_window` 提升 `pub(crate)`)

**前端**:
- `web/src/api/config.ts`(`requestPermission` 加 `openSettings?`)
- `web/src/lib/types.ts`(`PermissionRequestResult` / requestPermission 签名,如需)
- `web/src/App.tsx`(OnboardingGuard 自动 request + `PermissionNeededListener`)
