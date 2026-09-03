//! 中转访问（跳板机）单进程三节点端到端冒烟。
//!
//! Business Logic（为什么需要这个测试文件）:
//!     局域网存在发起方 A 无法直连目标 C、但共享可达跳板 B 的拓扑。中转链路横跨
//!     B 端透明转发（`/api/relay/*` + 白名单 + 并发闸 + 错误信封）、A 侧影子探测
//!     （`/api/relay/peers` 合成影子设备）与 C 端全部既有业务路由（health / fs /
//!     事件 NDJSON / 终端输入 WS）。必须在真实 TCP + 生产 middleware 栈（request_id
//!     → lan_socket_gate → browser_guard → expected_device_id → envelope → body
//!     limit）上证明三节点协作，而不是只靠 `net/routes/relay.rs` 的单节点 mock。
//!
//! Code Logic（这个文件做什么）:
//!     在同一进程内拉起三个**真实生产节点**（`build_app_state` 生产装配 +
//!     `start_backend_services(advertise=true, browse=false)`，即生产 axum Router
//!     完整 middleware 栈）：
//!     - A（发起方）：`relay.viaDeviceIds=[B]`，manual_peers=[B]；
//!     - B（跳板）：manual_peers=[C, ghost]（ghost 指向死端口，health 探测永不成功，
//!       永不入直连表 = "目标下线" 语义）；
//!     - C（目标）：无 manual_peers。
//!     直连表由生产 manual_peers 探测循环（health 探测 upsert）填充，影子表由生产
//!     影子探测循环填充；断言全部走 HTTP（含 headless CLI 的 control devices 快照）。
//!     端口全部为预占的随机空闲端口（不硬编码 62116），数据目录经
//!     `CC_PARTNER_DATA_DIR` 隔离到临时目录，不触碰真实 `~/.cc-partner`。
//!
//! 用例清单（9 个，各自独立 `#[tokio::test]`，共享 OnceLock fixture，串行运行）：
//!     1. 影子发现：B `/api/relay/peers` 报告 C；A 的 control devices 合并视图含
//!        C（viaDeviceId/viaDeviceName/online）且 shadows 条目正确。
//!     2. health 绑定经中转：`{B}/api/relay/{C}/api/health` + expected-device=C
//!        → 200 且 device_id == C（expected-device 绑定在 B、C 两跳守卫都通过）。
//!     3. workbench 只读链路：`{B}/api/relay/{C}/api/workbench/fs/roots` 与
//!        `/fs/list` 返回 C 的真实文件系统数据。
//!     4. 事件 NDJSON 经中转：`{B}/api/relay/{C}/api/workbench/events` 用 stale
//!        游标订阅读到显式 Gap 帧（ownerInstanceId/oldestAvailable/latest 字段）。
//!     5. 终端输入 WS 经中转：连接 `{B}/api/relay/{C}/api/workbench/
//!        terminal-input-stream`（子协议 `cc-partner.terminal-input.v1`），C 的
//!        生产网关回 Ready{deviceId:C}，ping/pong 帧端到端往返。
//!     6. 错误路径·目标下线：不在 B 直连表的目标转发 → 404 `relay_target_offline`。
//!     7. 错误路径·白名单外：`{B}/api/relay/{C}/api/sync/manifest` → 403
//!        `relay_path_not_allowed`。
//!     8. guard 语义：expected-device=A 打 relay 路径 → 409 `device_id_mismatch`；
//!        expected-device=C → 放行。
//!     9. 错误路径·跳板宕机（fail-closed）：对死端口（宕机跳板等价物）走生产
//!        `PeerClient::health_info` → `PeerCallError::Network`（"远端设备不在线"
//!        的生产错误分类）。
//!
//! 保真度边界（已知缺口，不遮蔽）：
//!     - A 侧客户端栈（`RemoteWorkbenchClient` / `device_base_url` 三段解析 /
//!       `parse_peer_response`）在 `commands`/`workbench`/`net` crate 私有模块内，
//!       集成测试无法直接驱动；A 以真实节点身份参与（影子探测循环 + devices 表 +
//!       control 快照），A→B→C 的线上行为用等价 HTTP 断言。
//!     - 用例 4 的 C 侧业务事件发布需要真实终端会话（无测试注入点），改用显式
//!       Gap 帧证明 NDJSON 流端到端；业务帧编码由 lib 单测覆盖。
//!     - 用例 6 的 502→offline 收敛（转发实测连接失败顺带置 offline）依赖
//!       "目标先健康后死亡"，进程内无法关停已启动的生产 HTTP server；该收敛由
//!       `net/routes/relay.rs::relay_marks_target_offline_after_unreachable` 单测
//!       覆盖，此处断言 offline 语义的最终 404 信封。
//!     - 用例 9 的设备解析层 fail-closed（影子 offline / via 缺失 → "远端设备
//!       不在线"）由 lib 内 `device_base_url_with_shadows` 单测覆盖；此处断言
//!       生产客户端 seam 的网络失败分类。
//!     - `start_backend_services(advertise=true)` 会注册真实 mDNS 服务（browse=false
//!       不浏览局域网、不会反向发现真实设备）；测试进程退出后注册自动消失。
//!
//! 运行方式（`CC_PARTNER_DATA_DIR` 切换是进程级的，必须单线程）：
//!     cargo test --locked --test relay_three_node_smoke -- --test-threads=1

use app_lib::backend::authority::CONTROL_SCHEMA_VERSION;
use app_lib::backend::control::{write_control_file, BackendControlFile};
use app_lib::backend::runtime::{build_app_state, start_backend_services};
use app_lib::backend::ui::HeadlessBackendUi;
use app_lib::{PeerCallError, PeerClient};
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as WsMessage;

/// 单用例网络断言预算（防悬挂；fixture 收敛轮询单独给更长预算）。
const CASE_BUDGET: Duration = Duration::from_secs(10);

/// 三节点固定身份：A 发起方 / B 跳板 / C 目标 / ghost 永不上线的假目标。
const ID_A: &str = "relay-smoke-initiator-a";
const ID_B: &str = "relay-smoke-jump-b";
const ID_C: &str = "relay-smoke-target-c";
const ID_GHOST: &str = "relay-smoke-ghost-offline";
const NAME_A: &str = "relay-smoke-initiator";
const NAME_B: &str = "relay-smoke-jump-host";
const NAME_C: &str = "relay-smoke-target";

/// A 节点 control 文件令牌（测试自写 control file，供 control devices 鉴权）。
const CONTROL_TOKEN: &str = "relay-smoke-control-token";
/// expected-device 绑定 header（`lan_guard::EXPECTED_DEVICE_ID_HEADER` 的线上契约）。
const EXPECTED_DEVICE_HEADER: &str = "x-cc-partner-expected-device-id";
/// 终端输入 WS 子协议（`TERMINAL_INPUT_SUBPROTOCOL` 的线上契约）。
const TERMINAL_INPUT_SUBPROTOCOL: &str = "cc-partner.terminal-input.v1";
/// 中转能力 token（relay 开启节点应在 health 宣告）。
const CAPABILITY_NET_RELAY: &str = "net.relay.v1";

/// 三节点运行时句柄（端口 + 临时目录 + 用例夹具路径）。
///
/// Business Logic（为什么需要这个结构）:
///     9 个用例共享同一套节点：生产探测循环是 15s 节奏、影子探测槽位是进程级
///     OnceLock（同进程只能启动一次），重复拉起节点既慢又拿不到第二个影子探测；
///     端口与临时目录必须在整个测试二进制生命周期内存活。
///
/// Code Logic（这个结构做什么）:
///     持有三节点 HTTP 端口、ghost 死端口、数据目录根（TempDir 进程尾清理）与
///     C 侧文件系统 marker 夹具路径。AppState 本体由生产任务持有，不在此存引用。
struct Nodes {
    _root: tempfile::TempDir,
    port_a: u16,
    port_b: u16,
    port_c: u16,
    port_ghost: u16,
    fs_marker_dir: PathBuf,
    fs_marker_subdir: PathBuf,
}

impl Nodes {
    /// 拼节点 base_url（`http://127.0.0.1:{port}`）。
    fn base_url(&self, port: u16) -> String {
        format!("http://127.0.0.1:{port}")
    }

    /// 失败诊断上下文（三节点端口与关键 URL），随断言消息输出。
    fn diagnostics(&self) -> String {
        format!(
            "A=127.0.0.1:{} B=127.0.0.1:{} C=127.0.0.1:{} ghost_port={} \
             relay_health={}/api/relay/{ID_C}/api/health \
             control_devices={}/api/backend/control/devices",
            self.port_a,
            self.port_b,
            self.port_c,
            self.port_ghost,
            self.base_url(self.port_b),
            self.base_url(self.port_a),
        )
    }
}

static NODES: OnceLock<Nodes> = OnceLock::new();

/// 共享服务器 runtime。
///
/// Business Logic（为什么需要这个函数）:
///     生产 `start_http_server` 用 `tokio::spawn` 启动 axum serve，任务落在调用时
///     的当前 runtime；用例各自的 `#[tokio::test]` runtime 会随用例结束销毁，
///     必须把 server 固定在一个泄漏的全局 runtime 上才能跨用例存活
///     （探测循环由生产代码落在 tauri 全局 runtime，与本 runtime 无关）。
///
/// Code Logic（这个函数做什么）:
///     惰性构建 multi-thread tokio runtime 并 `Box::leak`，返回 `'static` 引用。
fn server_runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<&'static tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        Box::leak(Box::new(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("构建三节点共享 tokio runtime"),
        ))
    })
}

/// 获取（或首次构建）三节点 fixture。
///
/// Business Logic（为什么需要这个函数）:
///     用例只应关心断言本身；节点拉起、直连/影子表收敛等待全部在首用例前完成，
///     保证每个用例的网络断言都在 10s 预算内。
///
/// Code Logic（这个函数做什么）:
///     `OnceLock::get_or_init` + 在独立 OS 线程上 `block_on(build_nodes())`——
///     测试线程处于某个 tokio runtime 上下文内，直接对另一个 runtime 调
///     `block_on` 会 panic，必须换线程。
fn nodes() -> &'static Nodes {
    NODES.get_or_init(|| {
        let rt = server_runtime();
        std::thread::scope(|scope| {
            scope
                .spawn(|| rt.block_on(build_nodes()))
                .join()
                .expect("三节点 fixture 构建失败（原始 panic 见上方输出）")
        })
    })
}

/// 构建三节点：写配置 → 依次 build_app_state → 起 advertise 服务 → 等收敛。
///
/// Business Logic（为什么需要这个函数）:
///     中转链路的全部生产参与者（manual_peers 探测、影子探测、relay 路由、
///     expected-device 守卫）都要在真实节点上就位；收敛由生产循环自行完成，
///     测试只轮询观测面（B 的 relay peers、A 的 control devices 快照）。
///
/// Code Logic（这个函数做什么）:
///     1. 预占 4 个随机空闲端口（bind 后立刻释放）并搭好 C 侧文件 marker；
///     2. 按 A → C → B 顺序切 `CC_PARTNER_DATA_DIR` 写 config.json 并
///        `build_app_state`（结束时 env 停在 A，供 control devices 鉴权读）；
///     3. 为 A 写 control file（自定 token）；
///     4. `start_backend_services(advertise=true, browse=false)` 按同一顺序起节点：
///        **A 必须第一个 start**——影子探测槽位是进程级 OnceLock，A 先 start 才能
///        以 A 的状态启动影子探测（C/B 先起会占走槽位，A 永远没有影子探测）；
///        C 在 B 之前起，B 的 manual 探测首轮即可把 C 入表；
///     5. health 身份校验三个端口 → 轮询 B 的 `/api/relay/peers` 报告 C →
///        轮询 A 的 control devices 出现 online 的 C 影子（A 起时 B 尚未入其
///        直连表，首轮探测标记 via offline，第 2/3 轮收敛，预算放宽到 75s）。
async fn build_nodes() -> Nodes {
    let root = tempfile::Builder::new()
        .prefix("cc-partner-relay-3node-")
        .tempdir()
        .expect("创建三节点临时根目录");
    let root_path = root.path().to_path_buf();
    let data_a = root_path.join("data-a");
    let data_b = root_path.join("data-b");
    let data_c = root_path.join("data-c");
    for dir in [&data_a, &data_b, &data_c] {
        std::fs::create_dir_all(dir).expect("创建节点数据目录");
    }

    // 预占随机端口：bind 后立刻释放的窗口极小；若被抢占，后续 health 身份校验
    // 会显式失败并输出诊断，不会静默错连。
    let (port_c, port_b, port_a, port_ghost) = (
        reserve_free_port(),
        reserve_free_port(),
        reserve_free_port(),
        reserve_free_port(),
    );

    // C 侧文件系统 marker：fs/list 经 B 中转后应原样带回这些真实条目。
    let fs_marker_dir = root_path.join("c-fs-marker");
    let fs_marker_subdir = fs_marker_dir.join("relay-marker-folder");
    std::fs::create_dir_all(&fs_marker_subdir).expect("创建 marker 子目录");
    std::fs::File::create(fs_marker_dir.join("relay-marker.txt"))
        .and_then(|mut file| file.write_all(b"relay-smoke"))
        .expect("创建 marker 文件");

    // 写配置并构建节点（顺序 C → B → A；`CC_PARTNER_DATA_DIR` 是进程级环境变量，
    // SAFETY: 全部用例在 --test-threads=1 下串行执行）。
    write_node_config(&data_c, ID_C, NAME_C, port_c, &[], &[]);
    std::env::set_var("CC_PARTNER_DATA_DIR", &data_c);
    let state_c = build_app_state(Arc::new(HeadlessBackendUi::new(data_c.clone())))
        .await
        .unwrap_or_else(|error| panic!("构建 C 节点 AppState 失败: {error}"));

    write_node_config(
        &data_b,
        ID_B,
        NAME_B,
        port_b,
        &[(host_loopback(), port_c), (host_loopback(), port_ghost)],
        &[],
    );
    std::env::set_var("CC_PARTNER_DATA_DIR", &data_b);
    let state_b = build_app_state(Arc::new(HeadlessBackendUi::new(data_b.clone())))
        .await
        .unwrap_or_else(|error| panic!("构建 B 节点 AppState 失败: {error}"));

    write_node_config(
        &data_a,
        ID_A,
        NAME_A,
        port_a,
        &[(host_loopback(), port_b)],
        &[ID_B],
    );
    std::env::set_var("CC_PARTNER_DATA_DIR", &data_a);
    let state_a = build_app_state(Arc::new(HeadlessBackendUi::new(data_a.clone())))
        .await
        .unwrap_or_else(|error| panic!("构建 A 节点 AppState 失败: {error}"));

    // A 的 control file：control devices 快照的 loopback+token 鉴权数据源
    // （env 已停在 A 的数据目录，写入与后续读取同一份）。
    write_control_file(&BackendControlFile {
        pid: std::process::id(),
        port: port_a,
        device_id: ID_A.to_string(),
        device_name: NAME_A.to_string(),
        started_at: Utc::now().to_rfc3339(),
        control_token: CONTROL_TOKEN.to_string(),
        control_schema_version: CONTROL_SCHEMA_VERSION,
        owner_instance_id: None,
        agent_hub_api_version: 0,
    })
    .expect("写入 A 节点 control file");

    // 依次启动生产服务组（HTTP server + mDNS advertise + manual/影子探测循环）。
    // browse=false：不浏览局域网，节点互发现只走 manual_peers（确定性）。
    // 顺序 A → C → B：A 先 start 独占进程级影子探测槽位；C 先于 B 上线，
    // B 的 manual 探测首轮即可把 C 入表。
    let served_a = start_backend_services(&state_a, true, false)
        .await
        .expect("启动 A 节点后端服务");
    assert_eq!(served_a, port_a, "A 节点应绑定预占端口（可能被抢占）");
    let served_c = start_backend_services(&state_c, true, false)
        .await
        .expect("启动 C 节点后端服务");
    assert_eq!(served_c, port_c, "C 节点应绑定预占端口（可能被抢占）");
    let served_b = start_backend_services(&state_b, true, false)
        .await
        .expect("启动 B 节点后端服务");
    assert_eq!(served_b, port_b, "B 节点应绑定预占端口（可能被抢占）");

    let nodes = Nodes {
        _root: root,
        port_a,
        port_b,
        port_c,
        port_ghost,
        fs_marker_dir,
        fs_marker_subdir,
    };

    // health 身份校验：确认各端口上跑的确实是期望节点（防端口漂移/抢占错连），
    // 同时验证 relay 开启节点宣告 net.relay.v1。
    let client = http_client();
    for (port, id, name) in [
        (port_a, ID_A, NAME_A),
        (port_b, ID_B, NAME_B),
        (port_c, ID_C, NAME_C),
    ] {
        let base = nodes.base_url(port);
        let health = poll_until(20, &format!("节点 {id} health 就绪"), || {
            let client = client.clone();
            let url = format!("{base}/api/health");
            async move {
                let value: serde_json::Value =
                    client.get(url).send().await.ok()?.json().await.ok()?;
                (value["device_id"] == id).then_some(value)
            }
        })
        .await;
        assert_eq!(
            health["device_name"],
            name,
            "节点名应匹配, {}",
            nodes.diagnostics()
        );
        assert_eq!(
            health["http_port"],
            port,
            "health 回报端口应与实际监听一致, {}",
            nodes.diagnostics()
        );
        let capabilities = health["capabilities"]
            .as_array()
            .expect("capabilities 数组");
        assert!(
            capabilities.iter().any(|c| c == CAPABILITY_NET_RELAY),
            "relay 开启节点应宣告 {CAPABILITY_NET_RELAY}, {}",
            nodes.diagnostics()
        );
    }

    // 收敛观测 1：B 的 relay peers 报告 C（B 的 manual 探测首轮即入表，几乎即时）。
    let base_b = nodes.base_url(port_b);
    let client_for_peers = client.clone();
    poll_until::<serde_json::Value, _, _>(30, "B /api/relay/peers 报告 C", move || {
        let client = client_for_peers.clone();
        let url = format!("{base_b}/api/relay/peers");
        async move {
            let value: serde_json::Value = client.get(url).send().await.ok()?.json().await.ok()?;
            let reported = value
                .as_array()?
                .iter()
                .any(|peer| peer["deviceId"] == ID_C && peer["online"] == true);
            reported.then_some(value)
        }
    })
    .await;

    // 收敛观测 2：A 的影子探测把 C 合成为影子（生产探测周期 15s：A 起时 B 尚未
    // 被其 manual 探测入表，首轮标记 via offline，第 2/3 轮收敛）。
    let base_a = nodes.base_url(port_a);
    let client_for_shadow = client.clone();
    poll_until::<serde_json::Value, _, _>(75, "A 影子表收敛（C via B online）", move || {
        let client = client_for_shadow.clone();
        let url = format!("{base_a}/api/backend/control/devices");
        async move {
            let value: serde_json::Value = client
                .post(url)
                .json(&serde_json::json!({ "controlToken": CONTROL_TOKEN }))
                .send()
                .await
                .ok()?
                .json()
                .await
                .ok()?;
            let devices = value["devices"].as_array()?;
            let shadow_ready = devices.iter().any(|device| {
                device["id"] == ID_C && device["viaDeviceId"] == ID_B && device["online"] == true
            });
            shadow_ready.then_some(value)
        }
    })
    .await;

    println!("[fixture] 三节点就绪: {}", nodes.diagnostics());
    nodes
}

/// 预占一个随机空闲 TCP 端口。
///
/// Business Logic（为什么需要这个函数）:
///     测试不得硬编码 62116，也不得依赖生产"占用递增"逻辑（会掩盖端口漂移）；
///     先向系统要一个空闲端口再交给 config，配合 health 身份校验兜底抢占。
///
/// Code Logic（这个函数做什么）:
///     std listener bind `127.0.0.1:0` 取系统分配端口，drop 释放后返回端口号。
fn reserve_free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("预留空闲端口")
        .local_addr()
        .expect("读取预留端口")
        .port()
}

/// 全部节点共用的 manual peer host（节点都在本机 loopback）。
fn host_loopback() -> &'static str {
    "127.0.0.1"
}

/// 写一份节点 config.json。
///
/// Business Logic（为什么需要这个函数）:
///     `build_app_state` 的生产装配从 `CC_PARTNER_DATA_DIR` 读取 config.json；
///     节点身份、端口、manual_peers（决定直连表）与 relay.viaDeviceIds（决定
///     影子探测）全部经该文件注入，不走任何测试专用构造。
///
/// Code Logic（这个函数做什么）:
///     最小字段集（snake_case 顶层）+ `manual_peers`（host/port 对）+
///     `relay.viaDeviceIds`（camelCase 内层）；db_path 固定在数据目录内
///     （满足隔离根约束），db_path/receive_dir 均落在 data_dir 下。
fn write_node_config(
    data_dir: &Path,
    device_id: &str,
    device_name: &str,
    http_port: u16,
    manual_peers: &[(&str, u16)],
    via_device_ids: &[&str],
) {
    let peers: Vec<serde_json::Value> = manual_peers
        .iter()
        .map(|(host, port)| serde_json::json!({ "host": host, "port": port }))
        .collect();
    let via: Vec<String> = via_device_ids.iter().map(|id| id.to_string()).collect();
    let config = serde_json::json!({
        "device_id": device_id,
        "device_name": device_name,
        "http_port": http_port,
        "receive_dir": data_dir.join("received").display().to_string(),
        "db_path": data_dir.join("data.db").display().to_string(),
        "screenshot_hotkey": "<cmd>+s",
        "prompt_optimizer_hotkey": "<ctrl>",
        "prompt_optimizer_fill_language": "zh",
        "manual_peers": peers,
        "relay": {
            "enabled": true,
            "viaDeviceIds": via,
            "ignoredTargetIds": [],
        },
    });
    std::fs::write(data_dir.join("config.json"), config.to_string()).expect("写入节点 config.json");
}

/// 无默认总超时的共享 HTTP client。
///
/// Business Logic（为什么需要这个函数）:
///     NDJSON 长流不能挂请求级总超时；超时纪律由调用方的 tokio timeout 控制。
///
/// Code Logic（这个函数做什么）:
///     构造仅带连接超时的 reqwest Client（每用例一个，连接池不复用节点间状态）。
fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(3))
        .build()
        .expect("构建测试 HTTP client")
}

/// 有界轮询：每 500ms 重试一次 attempt，直到命中或超时 panic。
///
/// Business Logic（为什么需要这个函数）:
///     直连/影子表收敛由 15s 生产探测循环驱动，测试必须轮询观测面而不是死等。
///
/// Code Logic（这个函数做什么）:
///     deadline 判定 + attempt 返回 `Option<T>`（None = 未命中）；超时 panic
///     携带场景描述。
async fn poll_until<T, F, Fut>(max_secs: u64, desc: &str, mut attempt: F) -> T
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<T>>,
{
    let deadline = Instant::now() + Duration::from_secs(max_secs);
    loop {
        if let Some(value) = attempt().await {
            return value;
        }
        assert!(Instant::now() < deadline, "等待超时（{max_secs}s）: {desc}");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// 单用例超时包装。
///
/// Business Logic（为什么需要这个函数）:
///     任何用例的网络断言都不得悬挂拖死整轮测试；超时立即 panic 带用例名。
///
/// Code Logic（这个函数做什么）:
///     `tokio::time::timeout(CASE_BUDGET, fut)` 包装，超时 panic。
async fn within_budget<T>(label: &str, fut: impl Future<Output = T>) -> T {
    tokio::time::timeout(CASE_BUDGET, fut)
        .await
        .unwrap_or_else(|_| panic!("用例预算（{CASE_BUDGET:?}）超时: {label}"))
}

/// 解析并断言 P2P 错误信封的稳定 code 与 request_id。
///
/// Business Logic（为什么需要这个函数）:
///     错误信封契约只承诺稳定 code / request_id，不承诺中文 message 文案；
///     断言必须钉住前者、放过后者。
///
/// Code Logic（这个函数做什么）:
///     JSON 解析 body，断言 `code` 字段等于期望值且 `request_id` 非空，返回整个
///     信封供附加断言。
fn assert_envelope_code(expected_code: &str, body_text: &str) -> serde_json::Value {
    let value: serde_json::Value = serde_json::from_str(body_text)
        .unwrap_or_else(|error| panic!("错误 body 应为 JSON 信封: {error}, body={body_text}"));
    assert_eq!(
        value["code"], expected_code,
        "信封 code 应匹配, body={body_text}"
    );
    assert!(
        value["request_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty()),
        "信封应携带 request_id, body={body_text}"
    );
    value
}

/// 在设备条目数组中按 id 查找条目。
///
/// Code Logic（这个函数做什么）:
///     线性查找 id 字段匹配的 JSON 对象；两类生产 DTO 字段名不同——
///     control devices 的 DeviceDto 用 `id`，`/api/relay/peers` 的
///     RelayPeerInfoDto 用 `deviceId`——两者都兼容，供各断言复用。
fn find_device<'a>(devices: &'a [serde_json::Value], id: &str) -> Option<&'a serde_json::Value> {
    devices
        .iter()
        .find(|device| device["id"] == id || device["deviceId"] == id)
}

/// 用例 1：影子发现 —— B 的 relay peers 报告 C；A 的 control devices 合并视图
/// 含 C 影子（viaDeviceId/viaDeviceName/online）且 shadows 清单一致。
///
/// Code Logic（这个断言做什么）:
///     GET `{B}/api/relay/peers` 断言 C 条目字段；POST `{A}/api/backend/control/devices`
///     断言 relay 配置、devices 合并条目（影子带 via 字段且 online）与 shadows 清单。
#[tokio::test]
async fn relay_case1_shadow_discovery_and_relay_peers() {
    let nodes = nodes();
    let client = http_client();
    within_budget("case1", async {
        let peers: serde_json::Value = client
            .get(format!("{}/api/relay/peers", nodes.base_url(nodes.port_b)))
            .send()
            .await
            .expect("B /api/relay/peers 应可达")
            .json()
            .await
            .expect("peers 应为 JSON");
        let peers = peers.as_array().expect("peers 数组");
        let entry = find_device(peers, ID_C)
            .unwrap_or_else(|| panic!("B 的 relay peers 应包含 C, peers={peers:?}"));
        assert_eq!(entry["deviceName"], NAME_C, "目标名应来自跳板转述");
        assert_eq!(entry["online"], true);
        assert_eq!(entry["protoVersion"], 1);
        assert!(
            !peers.iter().any(|peer| peer["deviceId"] == ID_B),
            "peers 不得包含跳板自身, peers={peers:?}"
        );

        let snapshot: serde_json::Value = client
            .post(format!(
                "{}/api/backend/control/devices",
                nodes.base_url(nodes.port_a)
            ))
            .json(&serde_json::json!({ "controlToken": CONTROL_TOKEN }))
            .send()
            .await
            .expect("A control devices 应可达")
            .json()
            .await
            .expect("control devices 应为 JSON");
        assert_eq!(snapshot["deviceId"], ID_A);
        let via_ids = snapshot["relay"]["viaDeviceIds"]
            .as_array()
            .expect("relay.viaDeviceIds 数组");
        assert!(
            via_ids.iter().any(|id| id == ID_B),
            "A 的 relay 配置应包含 via B, snapshot={snapshot:?}"
        );

        let devices = snapshot["devices"].as_array().expect("devices 数组");
        let shadow_view = find_device(devices, ID_C)
            .unwrap_or_else(|| panic!("A 设备列表应含 C 影子, snapshot={snapshot:?}"));
        assert_eq!(shadow_view["viaDeviceId"], ID_B, "影子应经 B 中转");
        assert_eq!(
            shadow_view["viaDeviceName"], NAME_B,
            "via 名用于「经 X 中转」展示"
        );
        assert_eq!(
            shadow_view["online"], true,
            "影子 online = via 可达 && 跳板报告 online"
        );
        assert_eq!(shadow_view["name"], NAME_C);
        assert!(
            find_device(devices, ID_B).is_some(),
            "A 直连表应含 B（manual peer 探测入表）, devices={devices:?}"
        );

        let shadows = snapshot["shadows"].as_array().expect("shadows 数组");
        let shadow = shadows
            .iter()
            .find(|shadow| shadow["targetDeviceId"] == ID_C)
            .unwrap_or_else(|| panic!("A shadows 清单应含 C, snapshot={snapshot:?}"));
        assert_eq!(shadow["viaDeviceId"], ID_B);
        assert_eq!(shadow["deviceName"], NAME_C);
        assert_eq!(shadow["online"], true);
        assert!(
            shadow["lastSeen"]
                .as_str()
                .is_some_and(|seen| !seen.is_empty()),
            "影子 lastSeen 应为 RFC3339 字符串"
        );
    })
    .await;
    println!("[pass] case1 影子发现 + relay peers");
}

/// 用例 2：health 绑定经中转 —— `{B}/api/relay/{C}/api/health` 携带
/// expected-device=C 应 200，且返回的 device_id == C。
///
/// Code Logic（这个断言做什么）:
///     经 B 的 relay 通配转发拿到 C 的生产 health 响应，断言身份、端口与全链
///     request_id header。
#[tokio::test]
async fn relay_case2_health_binding_via_relay() {
    let nodes = nodes();
    let client = http_client();
    within_budget("case2", async {
        let response = client
            .get(format!(
                "{}/api/relay/{ID_C}/api/health",
                nodes.base_url(nodes.port_b)
            ))
            .header(EXPECTED_DEVICE_HEADER, ID_C)
            .send()
            .await
            .expect("经中转的 health 请求应可达");
        assert_eq!(
            response.status().as_u16(),
            200,
            "经中转 health 应成功, {}",
            nodes.diagnostics()
        );
        assert!(
            response.headers().get("x-cc-request-id").is_some(),
            "响应应携带全链 request_id"
        );
        let health: serde_json::Value = response.json().await.expect("health JSON");
        assert_eq!(
            health["device_id"], ID_C,
            "经中转返回的必须是 C 的身份（expected-device 绑定通过）"
        );
        assert_eq!(health["http_port"], nodes.port_c, "C 的真实端口应回到 A");
        assert_eq!(health["ok"], true);
    })
    .await;
    println!("[pass] case2 health 绑定经中转");
}

/// 用例 3：workbench 只读链路 —— C 的真实文件系统数据经 B 中转回到 A。
///
/// Code Logic（这个断言做什么）:
///     GET `{B}/api/relay/{C}/api/workbench/fs/roots`（C 的真实根入口非空）与
///     POST `.../fs/list`（marker 目录）断言 200 + C 侧真实条目原样返回。
#[tokio::test]
async fn relay_case3_workbench_fs_read_via_relay() {
    let nodes = nodes();
    let client = http_client();
    let base = format!(
        "{}/api/relay/{ID_C}/api/workbench",
        nodes.base_url(nodes.port_b)
    );
    within_budget("case3", async {
        let roots: serde_json::Value = client
            .get(format!("{base}/fs/roots"))
            .header(EXPECTED_DEVICE_HEADER, ID_C)
            .send()
            .await
            .expect("经中转 fs/roots 应可达")
            .json()
            .await
            .expect("roots 应为 JSON");
        let roots = roots.as_array().expect("roots 数组");
        assert!(
            !roots.is_empty(),
            "C 的根入口不应为空, {}",
            nodes.diagnostics()
        );
        assert!(
            roots.iter().all(|root| root["path"].as_str().is_some()),
            "roots 条目应携带绝对 path"
        );

        let listing: serde_json::Value = client
            .post(format!("{base}/fs/list"))
            .header(EXPECTED_DEVICE_HEADER, ID_C)
            .json(&serde_json::json!({ "path": nodes.fs_marker_dir.display().to_string() }))
            .send()
            .await
            .expect("经中转 fs/list 应可达")
            .json()
            .await
            .expect("fs/list 应为 JSON");
        let entries = listing.as_array().expect("fs/list 数组");
        let subdir = entries
            .iter()
            .find(|entry| entry["name"] == "relay-marker-folder");
        assert!(
            subdir.is_some(),
            "C 的 marker 子目录应经 B 回到 A, entries={entries:?}"
        );
        let subdir = subdir.expect("marker 子目录存在");
        assert_eq!(subdir["kind"], "dir");
        assert_eq!(
            subdir["path"],
            nodes.fs_marker_subdir.display().to_string(),
            "条目 path 应是 C 侧绝对路径"
        );
        let file = entries
            .iter()
            .find(|entry| entry["name"] == "relay-marker.txt")
            .unwrap_or_else(|| panic!("marker 文件应在中转列表中, entries={entries:?}"));
        assert_eq!(file["kind"], "file");
    })
    .await;
    println!("[pass] case3 workbench 只读链路经中转");
}

/// 用例 4：事件 NDJSON 经中转 —— 以 stale 游标订阅
/// `{B}/api/relay/{C}/api/workbench/events`，首帧必须是显式 Gap。
///
/// Code Logic（这个断言做什么）:
///     携带异主 afterOwnerInstanceId + afterSequence 订阅经中转事件流，断言
///     content-type 为 application/x-ndjson，分块读取首个非空行并断言 Gap 的
///     ownerInstanceId / oldestAvailable / latest 协议字段。
#[tokio::test]
async fn relay_case4_events_ndjson_gap_via_relay() {
    let nodes = nodes();
    let client = http_client();
    within_budget("case4", async {
        let mut response = client
            .get(format!(
                "{}/api/relay/{ID_C}/api/workbench/events",
                nodes.base_url(nodes.port_b)
            ))
            .query(&[
                ("afterOwnerInstanceId", "relay-smoke-stale-owner"),
                ("afterSequence", "1"),
            ])
            .header(EXPECTED_DEVICE_HEADER, ID_C)
            .send()
            .await
            .expect("经中转事件流应可达");
        assert_eq!(response.status().as_u16(), 200);
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        assert!(
            content_type.starts_with("application/x-ndjson"),
            "事件流 content-type 应为 NDJSON, 实际={content_type}"
        );

        // 分块读首行（流式响应不能挂请求级总超时；逐块读用 tokio timeout 约束）。
        let mut buffer = String::new();
        let first_line = loop {
            let chunk = tokio::time::timeout(CASE_BUDGET, response.chunk())
                .await
                .expect("读取事件流首帧超时")
                .expect("读取事件流首帧失败")
                .expect("事件流在首帧前关闭");
            buffer.push_str(&String::from_utf8_lossy(&chunk));
            if let Some(position) = buffer.find('\n') {
                let line: String = buffer.drain(..=position).collect();
                let line = line.trim();
                if !line.is_empty() {
                    break line.to_string();
                }
            }
        };

        let frame: serde_json::Value = serde_json::from_str(&first_line)
            .unwrap_or_else(|error| panic!("首帧应为 JSON: {error}, line={first_line}"));
        assert_eq!(
            frame["type"], "gap",
            "异主 stale 游标首帧必须是显式 Gap, line={first_line}"
        );
        assert!(
            frame["payload"]["ownerInstanceId"]
                .as_str()
                .is_some_and(|owner| !owner.is_empty()),
            "Gap 应携带 C 的 ownerInstanceId, line={first_line}"
        );
        assert!(
            frame["payload"]["oldestAvailable"].is_u64() && frame["payload"]["latest"].is_u64(),
            "Gap 应携带 oldestAvailable/latest 数值字段, line={first_line}"
        );
        drop(response);
    })
    .await;
    println!("[pass] case4 事件 NDJSON Gap 帧经中转");
}

/// 用例 5：终端输入 WS 经中转 —— 连接
/// `{B}/api/relay/{C}/api/workbench/terminal-input-stream`，子协议端到端协商，
/// C 的生产网关接受 Hello 回 Ready{deviceId:C}，ping/pong 帧双向穿透。
///
/// Code Logic（这个断言做什么）:
///     tungstenite 带子协议握手 → 断言协商结果 → hello/ready 握手（身份为 C）→
///     ping/pong 往返（帧端到端到达）→ 主动关闭。
#[tokio::test]
async fn relay_case5_terminal_input_ws_via_relay() {
    let nodes = nodes();
    within_budget("case5", async {
        let ws_url = format!(
            "ws://127.0.0.1:{}/api/relay/{ID_C}/api/workbench/terminal-input-stream",
            nodes.port_b
        );
        let mut request = ws_url
            .into_client_request()
            .expect("构造终端输入 WS 握手请求");
        request.headers_mut().insert(
            "sec-websocket-protocol",
            TERMINAL_INPUT_SUBPROTOCOL
                .parse()
                .expect("子协议 header 值"),
        );
        let (mut socket, response) = tokio_tungstenite::connect_async(request)
            .await
            .expect("经中转终端 WS 应连接成功");
        assert_eq!(
            response.headers().get("sec-websocket-protocol").unwrap(),
            TERMINAL_INPUT_SUBPROTOCOL,
            "子协议应经 B 桥与 C 端到端协商一致"
        );

        socket
            .send(WsMessage::Text(
                serde_json::json!({ "type": "hello", "clientId": "relay-smoke-a" }).to_string(),
            ))
            .await
            .expect("发送 hello 帧");
        let ready = tokio::time::timeout(CASE_BUDGET, socket.next())
            .await
            .expect("等待 ready 帧超时")
            .expect("WS 已关闭")
            .expect("读取 ready 帧失败");
        let ready_text = match ready {
            WsMessage::Text(text) => text,
            other => panic!("应收到文本 ready 帧, 实际: {other:?}"),
        };
        let ready_frame: serde_json::Value = serde_json::from_str(&ready_text)
            .unwrap_or_else(|error| panic!("ready 帧应为 JSON: {error}, frame={ready_text}"));
        assert_eq!(ready_frame["type"], "ready", "frame={ready_text}");
        assert_eq!(
            ready_frame["deviceId"], ID_C,
            "ready 身份必须是 C（网关在 B 中转另一端）, frame={ready_text}"
        );

        socket
            .send(WsMessage::Text(
                serde_json::json!({ "type": "ping", "nonce": "relay-smoke-n1" }).to_string(),
            ))
            .await
            .expect("发送 ping 帧");
        let pong = tokio::time::timeout(CASE_BUDGET, socket.next())
            .await
            .expect("等待 pong 帧超时")
            .expect("WS 已关闭")
            .expect("读取 pong 帧失败");
        let pong_text = match pong {
            WsMessage::Text(text) => text,
            other => panic!("应收到文本 pong 帧, 实际: {other:?}"),
        };
        let pong_frame: serde_json::Value = serde_json::from_str(&pong_text)
            .unwrap_or_else(|error| panic!("pong 帧应为 JSON: {error}, frame={pong_text}"));
        assert_eq!(pong_frame["type"], "pong", "frame={pong_text}");
        assert_eq!(pong_frame["nonce"], "relay-smoke-n1");

        let _ = socket.close(None).await;
    })
    .await;
    println!("[pass] case5 终端输入 WS 经中转");
}

/// 用例 6：错误路径·目标下线 —— 不在 B 直连表（= offline 语义：生产表内只保留
/// health 探测成功的设备）的目标经中转访问必须 404 `relay_target_offline` 信封。
///
/// Code Logic（这个断言做什么）:
///     ghost（指向死端口的 manual peer，探测失败永不入表）与从未存在过的
///     device_id 各请求一次，断言 404 + 稳定 code + request_id。
#[tokio::test]
async fn relay_case6_target_offline_envelope() {
    let nodes = nodes();
    let client = http_client();
    within_budget("case6", async {
        for device_id in [ID_GHOST, "relay-smoke-never-existed"] {
            let response = client
                .get(format!(
                    "{}/api/relay/{device_id}/api/health",
                    nodes.base_url(nodes.port_b)
                ))
                .send()
                .await
                .expect("relay 转发拒绝响应应可达");
            assert_eq!(
                response.status().as_u16(),
                404,
                "下线目标应 404, device={device_id}"
            );
            let body = response.text().await.expect("读取错误 body");
            assert_envelope_code("relay_target_offline", &body);
        }
    })
    .await;
    println!("[pass] case6 目标下线 relay_target_offline");
}

/// 用例 7：错误路径·白名单外 —— `/api/sync/*`、`/api/prompts` 不在中转白名单，
/// 必须 403 `relay_path_not_allowed`（双向同步类流量不得进入跳板拓扑）。
///
/// Code Logic（这个断言做什么）:
///     对在线目标请求两条白名单外路径，断言 403 信封。
#[tokio::test]
async fn relay_case7_path_not_allowed() {
    let nodes = nodes();
    let client = http_client();
    within_budget("case7", async {
        for path in ["/api/sync/manifest", "/api/prompts"] {
            let response = client
                .get(format!(
                    "{}/api/relay/{ID_C}{path}",
                    nodes.base_url(nodes.port_b)
                ))
                .send()
                .await
                .expect("白名单拒绝响应应可达");
            assert_eq!(
                response.status().as_u16(),
                403,
                "白名单外路径应 403, path={path}"
            );
            let body = response.text().await.expect("读取错误 body");
            assert_envelope_code("relay_path_not_allowed", &body);
        }
    })
    .await;
    println!("[pass] case7 白名单外 relay_path_not_allowed");
}

/// 用例 8：guard 语义 —— relay 路径上 expected-device 与 URL 目标段比对：
/// 带 A 的 id 打 `{B}/api/relay/{C}/...` → 409 `device_id_mismatch`；
/// 带 C 的 id → 放行（B 守卫与 C 守卫两跳一致）。
///
/// Code Logic（这个断言做什么）:
///     分别以错误/正确 expected-device 请求经中转 health，断言 409 信封与 200。
#[tokio::test]
async fn relay_case8_expected_device_guard() {
    let nodes = nodes();
    let client = http_client();
    let url = format!(
        "{}/api/relay/{ID_C}/api/health",
        nodes.base_url(nodes.port_b)
    );
    within_budget("case8", async {
        let mismatch = client
            .get(&url)
            .header(EXPECTED_DEVICE_HEADER, ID_A)
            .send()
            .await
            .expect("guard 拒绝响应应可达");
        assert_eq!(
            mismatch.status().as_u16(),
            409,
            "expected-device 与 relay 目标不一致应 409"
        );
        let body = mismatch.text().await.expect("读取错误 body");
        assert_envelope_code("device_id_mismatch", &body);

        let passed = client
            .get(&url)
            .header(EXPECTED_DEVICE_HEADER, ID_C)
            .send()
            .await
            .expect("guard 放行响应应可达");
        assert_eq!(passed.status().as_u16(), 200);
        let health: serde_json::Value = passed.json().await.expect("health JSON");
        assert_eq!(health["device_id"], ID_C);
    })
    .await;
    println!("[pass] case8 expected-device guard 语义");
}

/// 用例 9：错误路径·跳板宕机 fail-closed —— 跳板不可达时，A 侧生产客户端
/// seam（`PeerClient`）必须给出网络级失败分类（`PeerCallError::Network`，即
/// UI "远端设备不在线" 的来源分类），绝不把失败伪装成业务成功。
///
/// Code Logic（这个断言做什么）:
///     用 fixture 预占的死端口（宕机跳板等价物：TCP 无监听）走生产
///     `PeerClient::health_info` 断言 `PeerCallError::Network`；并用同一 client
///     对存活 B 的 health 成功作对照。解析层 fail-closed 由 lib 内
///     `device_base_url_with_shadows` 单测覆盖（见文件头保真度边界）。
#[tokio::test]
async fn relay_case9_jump_host_down_fail_closed() {
    let nodes = nodes();
    let peer_client = PeerClient::new();
    within_budget("case9", async {
        let dead = peer_client
            .health_info(&format!("http://127.0.0.1:{}", nodes.port_ghost))
            .await;
        match dead {
            Err(PeerCallError::Network { url, .. }) => {
                assert!(
                    url.contains(&nodes.port_ghost.to_string()),
                    "Network 错误应指向宕机跳板端口, url={url}"
                );
            }
            other => panic!(
                "宕机跳板应产生 Network 分类失败（fail-closed）, 实际: {other:?}, {}",
                nodes.diagnostics()
            ),
        }

        // 对照：同一生产 client 对存活 B 的 health 成功，证明失败确因跳板宕机。
        let alive = peer_client
            .health_info(&nodes.base_url(nodes.port_b))
            .await
            .expect("存活跳板 health 应成功");
        assert_eq!(alive.device_id, ID_B);
    })
    .await;
    println!("[pass] case9 跳板宕机 fail-closed（PeerCallError::Network）");
}
