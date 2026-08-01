use serde::{Deserialize, Serialize};

/// 输入监控授权状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InputMonitoringState {
    /// IOHID 或公开 CoreGraphics ListenEvent 预检明确报告已授权。
    Granted,
    /// 当前进程已显式请求但公开 ListenEvent 预检仍未授权。
    Denied,
    /// IOHID 尚未记录决定，或 macOS 26 在本进程请求前返回假 Denied。
    NotDetermined,
    /// 系统返回未知状态；用户仍可尝试在系统设置中手动添加当前应用。
    Unavailable,
}

/// 输入监控权限的前端 DTO。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InputMonitoringPermissionState {
    /// 兼容消费方的快速判据；仅 `state=granted` 时为 true。
    pub granted: bool,
    /// 用于决定 Welcome 显示“请求授权”还是“打开设置”。
    pub state: InputMonitoringState,
}

impl From<InputMonitoringState> for InputMonitoringPermissionState {
    fn from(state: InputMonitoringState) -> Self {
        Self {
            granted: state == InputMonitoringState::Granted,
            state,
        }
    }
}

/// 把公开 IOHID 返回值映射为产品四态。
///
/// 未知系统值返回 `Unavailable`，不猜测、不假绿；是否固定签名不参与可用性判断。
pub(crate) fn state_from_raw(raw: u32) -> InputMonitoringState {
    match raw {
        0 => InputMonitoringState::Granted,
        1 => InputMonitoringState::Denied,
        2 => InputMonitoringState::NotDetermined,
        _ => InputMonitoringState::Unavailable,
    }
}

/// 输入监控 Request 路径本次真正执行的操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum InputMonitoringOperation {
    /// 调用了公开 ListenEvent Request 登记路径。
    Request,
    /// 当前状态不允许请求，未产生系统副作用。
    Noop,
}

/// 输入监控显式 Request 的前后状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputMonitoringRequestResult {
    pub operation: InputMonitoringOperation,
    pub before: InputMonitoringState,
    pub after: InputMonitoringState,
}

/// 隔离公开 IOHID / CoreGraphics ListenEvent 系统调用，便于用纯内存 provider 验证
/// 状态机而不触碰宿主 TCC。
pub(crate) trait InputMonitoringAccessProvider {
    fn check(&self) -> u32;
    fn preflight_listen_event(&self) -> bool {
        false
    }
    fn requested_in_process(&self) -> bool {
        false
    }
    fn mark_requested_in_process(&self) {}
    fn request_listen_event(&self) -> bool {
        false
    }
    fn request(&self) -> bool;
}

/// 使用指定 provider 查询输入监控状态，不产生系统副作用。
pub(crate) fn check_with_provider<P: InputMonitoringAccessProvider>(
    provider: &P,
) -> InputMonitoringPermissionState {
    let raw = provider.check();
    if raw == 0 || provider.preflight_listen_event() {
        return InputMonitoringState::Granted.into();
    }
    match raw {
        1 if !provider.requested_in_process() => InputMonitoringState::NotDetermined.into(),
        _ => state_from_raw(raw).into(),
    }
}

/// 在指定公开系统 API provider 上执行一次显式 Request 状态机。
///
/// 仅 `NotDetermined` 调 Request；依次调用 CoreGraphics 与 IOHID 两条公开 ListenEvent
/// 请求路径。Denied/Granted/Unavailable 都是无副作用 noop。固定签名与 ad-hoc 构建
/// 共用该状态机；未自动登记时由前端引导用户在系统设置中手动添加当前 `.app`。
pub(crate) fn request_with_provider<P: InputMonitoringAccessProvider>(
    provider: &P,
) -> InputMonitoringRequestResult {
    let before = check_with_provider(provider).state;
    if before != InputMonitoringState::NotDetermined {
        return InputMonitoringRequestResult {
            operation: InputMonitoringOperation::Noop,
            before,
            after: before,
        };
    }

    provider.mark_requested_in_process();
    let _ = provider.request_listen_event();
    let _ = provider.request();
    InputMonitoringRequestResult {
        operation: InputMonitoringOperation::Request,
        before,
        after: check_with_provider(provider).state,
    }
}

#[cfg(target_os = "macos")]
#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOHIDCheckAccess(request_type: u32) -> u32;
    fn IOHIDRequestAccess(request_type: u32) -> bool;
}

#[cfg(target_os = "macos")]
#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPreflightListenEventAccess() -> bool;
    fn CGRequestListenEventAccess() -> bool;
}

#[cfg(target_os = "macos")]
const K_IOHID_REQUEST_TYPE_LISTEN_EVENT: u32 = 1;

#[cfg(target_os = "macos")]
struct SystemInputMonitoringProvider;

#[cfg(target_os = "macos")]
static REQUESTED_INPUT_MONITORING_IN_PROCESS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[cfg(target_os = "macos")]
impl InputMonitoringAccessProvider for SystemInputMonitoringProvider {
    fn check(&self) -> u32 {
        unsafe { IOHIDCheckAccess(K_IOHID_REQUEST_TYPE_LISTEN_EVENT) }
    }

    fn request(&self) -> bool {
        unsafe { IOHIDRequestAccess(K_IOHID_REQUEST_TYPE_LISTEN_EVENT) }
    }

    fn request_listen_event(&self) -> bool {
        unsafe { CGRequestListenEventAccess() }
    }

    fn preflight_listen_event(&self) -> bool {
        unsafe { CGPreflightListenEventAccess() }
    }

    fn requested_in_process(&self) -> bool {
        REQUESTED_INPUT_MONITORING_IN_PROCESS.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn mark_requested_in_process(&self) {
        REQUESTED_INPUT_MONITORING_IN_PROCESS.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// 查询当前进程的输入监控四态。
///
/// 固定签名与 ad-hoc `.app` 都可查询；其它平台保持既有跨平台语义，视为已授权。
#[cfg(target_os = "macos")]
pub fn check_input_monitoring_state() -> InputMonitoringPermissionState {
    check_with_provider(&SystemInputMonitoringProvider)
}

#[cfg(not(target_os = "macos"))]
pub fn check_input_monitoring_state() -> InputMonitoringPermissionState {
    InputMonitoringState::Granted.into()
}

/// 显式请求输入监控授权。
///
/// macOS 上任何 `.app` 的 NotDetermined 状态都会调用公开 ListenEvent Request。
#[cfg(target_os = "macos")]
pub fn request_input_monitoring_access() -> InputMonitoringRequestResult {
    request_with_provider(&SystemInputMonitoringProvider)
}

#[cfg(not(target_os = "macos"))]
pub fn request_input_monitoring_access() -> InputMonitoringRequestResult {
    InputMonitoringRequestResult {
        operation: InputMonitoringOperation::Noop,
        before: InputMonitoringState::Granted,
        after: InputMonitoringState::Granted,
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::{
        check_with_provider, request_with_provider, state_from_raw, InputMonitoringAccessProvider,
        InputMonitoringOperation, InputMonitoringState,
    };

    struct FakeProvider {
        state: Cell<u32>,
        state_after_request: u32,
        request_calls: Cell<usize>,
        request_order: RefCell<Vec<&'static str>>,
        listen_event_granted: Cell<bool>,
        requested_in_process: Cell<bool>,
    }

    impl FakeProvider {
        fn new(state: u32, state_after_request: u32) -> Self {
            Self {
                state: Cell::new(state),
                state_after_request,
                request_calls: Cell::new(0),
                request_order: RefCell::new(Vec::new()),
                listen_event_granted: Cell::new(false),
                requested_in_process: Cell::new(false),
            }
        }
    }

    impl InputMonitoringAccessProvider for FakeProvider {
        fn check(&self) -> u32 {
            self.state.get()
        }

        fn request(&self) -> bool {
            self.request_order.borrow_mut().push("iohid");
            self.request_calls.set(self.request_calls.get() + 1);
            self.state.set(self.state_after_request);
            true
        }

        fn request_listen_event(&self) -> bool {
            self.request_order.borrow_mut().push("coreGraphics");
            false
        }

        fn preflight_listen_event(&self) -> bool {
            self.listen_event_granted.get()
        }

        fn requested_in_process(&self) -> bool {
            self.requested_in_process.get()
        }

        fn mark_requested_in_process(&self) {
            self.requested_in_process.set(true);
        }
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Welcome 必须区分已授权、已拒绝、未登记和构建不可用，不能再用一个 bool
    ///     把“未登记”和“身份不稳定”混成同一条错误引导。
    ///
    /// Code Logic（这个测试做什么）:
    ///     把公开 IOHID API 的三个稳定返回值映射到前端四态；未知值 fail closed。
    #[test]
    fn maps_iohid_state_for_every_macos_app_subject() {
        assert_eq!(state_from_raw(0), InputMonitoringState::Granted);
        assert_eq!(state_from_raw(1), InputMonitoringState::Denied);
        assert_eq!(state_from_raw(2), InputMonitoringState::NotDetermined);
        assert_eq!(state_from_raw(99), InputMonitoringState::Unavailable);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     已拒绝时系统不会再次弹授权框；产品必须等待用户点“打开系统设置”，不能偷偷
    ///     reset TCC、重签或反复请求。
    ///
    /// Code Logic（这个测试做什么）:
    ///     注入 IOHID=Denied，断言 Request 操作为 noop 且 provider 从未收到 request。
    #[test]
    fn denied_request_is_noop() {
        let provider = FakeProvider::new(1, 0);
        provider.requested_in_process.set(true);
        let result = request_with_provider(&provider);

        assert_eq!(result.operation, InputMonitoringOperation::Noop);
        assert_eq!(result.before, InputMonitoringState::Denied);
        assert_eq!(result.after, InputMonitoringState::Denied);
        assert_eq!(provider.request_calls.get(), 0);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     只有“未决定”状态允许请求一次，让 macOS 登记稳定签名的应用主体。
    ///
    /// Code Logic（这个测试做什么）:
    ///     注入 IOHID=NotDetermined，请求后切到 Denied，断言只调用一次公开 Request 并复查状态。
    #[test]
    fn not_determined_requests_exactly_once_then_rechecks() {
        let provider = FakeProvider::new(2, 1);
        let result = request_with_provider(&provider);

        assert_eq!(result.operation, InputMonitoringOperation::Request);
        assert_eq!(result.before, InputMonitoringState::NotDetermined);
        assert_eq!(result.after, InputMonitoringState::Denied);
        assert_eq!(provider.request_calls.get(), 1);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     macOS 26 上 IOHID Request 可能直接把状态改成 Denied，却既不弹系统中转框也不把
    ///     应用登记进输入监控列表；一旦先走 IOHID，后续公开 Request 已失去首次登记机会。
    ///
    /// Code Logic（这个测试做什么）:
    ///     模拟 CoreGraphics 首次登记未直接授权、IOHID 作为后备请求的路径，锁定两个公开
    ///     ListenEvent API 的调用顺序必须是 CoreGraphics 在前、IOHID 在后。
    #[test]
    fn not_determined_registers_with_core_graphics_before_iohid_fallback() {
        let provider = FakeProvider::new(2, 1);

        let result = request_with_provider(&provider);

        assert_eq!(result.operation, InputMonitoringOperation::Request);
        assert_eq!(result.before, InputMonitoringState::NotDetermined);
        assert_eq!(result.after, InputMonitoringState::Denied);
        assert_eq!(
            provider.request_order.borrow().as_slice(),
            ["coreGraphics", "iohid"]
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     macOS 26 首次检查可能直接错报 Denied；进入显式请求后不能因为该假状态而跳过
    ///     Apple DTS 推荐的 IOHID Request 后备路径。
    ///
    /// Code Logic（这个测试做什么）:
    ///     模拟进程首次检查 raw=Denied，断言用户显式请求时仍按 CoreGraphics → IOHID
    ///     顺序各调用一次，最终才稳定为 Denied 并交给手动「+」登记兜底。
    #[test]
    fn false_denied_first_request_still_calls_both_public_apis() {
        let provider = FakeProvider::new(1, 1);

        let result = request_with_provider(&provider);

        assert_eq!(result.operation, InputMonitoringOperation::Request);
        assert_eq!(result.before, InputMonitoringState::NotDetermined);
        assert_eq!(result.after, InputMonitoringState::Denied);
        assert_eq!(
            provider.request_order.borrow().as_slice(),
            ["coreGraphics", "iohid"]
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     ad-hoc 构建同样能够使用输入监控；如果 macOS 没有自动登记，用户仍可在系统设置
    ///     中手动选择当前 `.app`，所以后端不能按签名或 Bundle ID 禁止公开 Request。
    ///
    /// Code Logic（这个测试做什么）:
    ///     注入 NotDetermined，断言所有 macOS `.app` 都能进入公开 Request 路径。
    #[test]
    fn ad_hoc_app_subject_can_request_input_monitoring() {
        let provider = FakeProvider::new(2, 0);
        let result = request_with_provider(&provider);

        assert_eq!(result.operation, InputMonitoringOperation::Request);
        assert_eq!(result.before, InputMonitoringState::NotDetermined);
        assert_eq!(result.after, InputMonitoringState::Granted);
        assert_eq!(provider.request_calls.get(), 1);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     `granted` 兼容字段只能由四态中的 Granted 推导，NotDetermined 不能因为系统 API
    ///     可调用就被误判为已授权。
    ///
    /// Code Logic（这个测试做什么）:
    ///     查询受支持主体的 IOHID=NotDetermined，断言 DTO 同时携带 state 且 granted=false。
    #[test]
    fn check_reports_not_determined_as_not_granted() {
        let provider = FakeProvider::new(2, 0);
        let result = check_with_provider(&provider);

        assert_eq!(result.state, InputMonitoringState::NotDetermined);
        assert!(!result.granted);
        assert_eq!(provider.request_calls.get(), 0);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     macOS 26 会在刚重置、尚未请求时把 IOHID ListenEvent 错报为 Denied；若直接相信该值，
    ///     Welcome 只会显示“打开系统设置”，用户永远没有触发登记弹窗的机会。
    ///
    /// Code Logic（这个测试做什么）:
    ///     注入 IOHID=Denied、CG 未授权且当前进程尚未请求，断言查询仍给出可请求的
    ///     NotDetermined，而不是提前锁死为 Denied。
    #[test]
    fn iohid_denied_before_process_request_remains_requestable() {
        let provider = FakeProvider::new(1, 1);

        let result = check_with_provider(&provider);

        assert_eq!(result.state, InputMonitoringState::NotDetermined);
        assert!(!result.granted);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     macOS 26 的 IOHID 查询即使在 ListenEvent 已授权后仍可能返回 Denied；权限页不能因此
    ///     永久假红并阻止健康采样。
    ///
    /// Code Logic（这个测试做什么）:
    ///     注入 IOHID=Denied 但公开 CoreGraphics ListenEvent preflight=true，断言最终状态为 Granted。
    #[test]
    fn core_graphics_preflight_repairs_iohid_false_denial() {
        let provider = FakeProvider::new(1, 1);
        provider.listen_event_granted.set(true);

        let result = check_with_provider(&provider);

        assert_eq!(result.state, InputMonitoringState::Granted);
        assert!(result.granted);
    }
}
