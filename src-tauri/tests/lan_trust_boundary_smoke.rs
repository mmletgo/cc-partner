//! 固定 LAN trust boundary 集成冒烟。
//!
//! 覆盖绑定 loopback 服务器上的无凭据业务读写、Host/Origin、stop loopback+token、
//! 资源上限，以及标注为 INJECTED 的 denied/forwarded peer 证据。
//! 真实多机 mDNS / 手机 QR / 公网 peer 路径见 `docs/development/testing.md` 的 NOT VERIFIED。
//! Browser L1 Playwright 归 S6，本文件不重复创建 Playwright journey。

/// Business Logic（为什么需要这个测试）:
///     跨平台 CI 需要证明 S1 固定 LAN 边界在真实绑定端口 + middleware 栈上可复现，
///     且不得用 X-Forwarded-For 伪装成生产多机证据。
///
/// Code Logic（这个测试做什么）:
///     调用 `app_lib::lan_trust_boundary_harness::run_lan_trust_boundary_smoke` 跑完整矩阵。
#[test]
fn lan_trust_boundary_bound_server_matrix() {
    app_lib::lan_trust_boundary_harness::run_lan_trust_boundary_smoke();
}
