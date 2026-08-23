//! agent_hub_user_mirror_smoke — L2 用户级全 Agent 镜像
//! Evidence: L2-AGENT-HUB-USER-MIRROR-001, L2-AGENT-HUB-USER-MIRROR-002,
//!           L2-AGENT-HUB-USER-MIRROR-003
//!
//! Business Logic（为什么需要这个测试文件）:
//!     用户级镜像必须在双隔离 `data_dir` 下证明：全 Agent inventory、Claude 三槽+
//!     CLAUDE.md+skill 对齐、dest 多余 Skill/MCP 消失、Grok 不写仓库 `AGENTS.md`、
//!     MCP DTO 无 secret、health 宣告 `agent-hub.user-mirror.v1`、缺能力零旧路由命中；
//!     单 Agent 写失败 partial 且不回滚；Skill detach / Plugin disable / MCP 删键。
//!     双进程 backend / 真机 mDNS 仍为 L3。
//!
//! Code Logic（这个文件做什么）:
//!     library-level process smoke（对齐 portable_pull_smoke DualEnv + frozen axum peer）:
//!     - 001：源 freeze + dest apply + capable/missing loopback 计数器
//!     - 002：dest `.codex` 只读注入写失败 → partial + 同 clientRequestId 重放
//!     - 003：dest extras 策略（detach / disable 留目录 / 删 MCP 键）
//!
//! NOT VERIFIED（本 smoke 不宣称）:
//!     - 真实双主机 mDNS / 多进程 backend / LAN 身份认证（L3）
//!     - 打包 GUI / 全平台矩阵

use app_lib::agent_catalog::all_hub_targets;
use app_lib::agent_hub::portable_inventory::{
    invalidate_portable_inventory_cache, PortableAssetKind,
};
use app_lib::agent_hub::targets::portable::claude_user_mcp_config_path;
use app_lib::agent_hub::targets::TargetEnvironment;
use app_lib::agent_hub::user_mirror::{
    apply_user_mirror, build_local_user_mirror_inventory, freeze_user_mirror_selection,
    get_user_mirror, preview_from_two_inventories, source_read_user_mirror_object_chunk,
    ApplyUserMirrorRequest, UserMirrorDirection, UserMirrorInventoryDto, UserMirrorItemState,
    UserMirrorPlanDto, UserMirrorPlanRecord, UserMirrorResultDto, UserMirrorSelectionQuery,
    UserMirrorSelectionResponse, USER_MIRROR_CAPABILITY_UNSUPPORTED,
};
use app_lib::backend::runtime::build_app_state;
use app_lib::backend::ui::HeadlessBackendUi;
use app_lib::server_protocol_info;
use app_lib::{AgentTarget, PeerCallError, PeerClient, CAPABILITY_AGENT_HUB_V1};
use axum::extract::{Path, Query, State};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use std::fs;
use std::path::{Path as FsPath, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex as AsyncMutex;

const CREDENTIAL: &str = "plain-fixture-user-mirror-secret";
const EVIDENCE_001: &str = "L2-AGENT-HUB-USER-MIRROR-001";
const EVIDENCE_002: &str = "L2-AGENT-HUB-USER-MIRROR-002";
const EVIDENCE_003: &str = "L2-AGENT-HUB-USER-MIRROR-003";
const SOURCE_DEVICE: &str = "user-mirror-source-device";
const DEST_DEVICE: &str = "user-mirror-dest-device";
const CAPABILITY_USER_MIRROR_V1: &str = "agent-hub.user-mirror.v1";
const CAPABILITY_PORTABLE_PULL_V1: &str = "agent-hub.portable-pull.v1";
const SRC_CLAUDE: &str = "FROM-SRC-CLAUDE";
const SRC_CODEX: &str = "FROM-SRC-CODEX";
const SRC_GROK: &str = "FROM-SRC-GROK";
const KEEP_SKILL_BODY: &str = "KEEP-SKILL-BODY";
const REPO_AGENTS: &str = "REPO-AGENTS-MUST-STAY";

struct DualEnv {
    _root: tempfile::TempDir,
    source_data: PathBuf,
    source_home: PathBuf,
    dest_data: PathBuf,
    dest_home: PathBuf,
}

/// Business Logic: L2 不得触碰开发者真实 `~/.cc-partner` 与 HOME 配置。
///
/// Code Logic: tempfile 下拆 source/dest data_dir + HOME，并预建 objects 目录。
fn setup_dual_env() -> DualEnv {
    let root = tempfile::Builder::new()
        .prefix("cc-partner-user-mirror-")
        .tempdir()
        .expect("tempdir");
    let source_data = root.path().join("source-data");
    let source_home = root.path().join("source-home");
    let dest_data = root.path().join("dest-data");
    let dest_home = root.path().join("dest-home");
    for p in [
        source_data.join("agent-hub/objects"),
        dest_data.join("agent-hub/objects"),
        source_home.clone(),
        dest_home.clone(),
    ] {
        fs::create_dir_all(&p).expect("mkdir");
    }
    DualEnv {
        _root: root,
        source_data,
        source_home,
        dest_data,
        dest_home,
    }
}

fn write(path: &FsPath, text: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("parent");
    }
    fs::write(path, text).expect("write");
}

fn write_skill(dir: &FsPath, name: &str, body: &str) {
    write(
        &dir.join(name).join("SKILL.md"),
        &format!("---\nname: {name}\ndescription: user-mirror-smoke\n---\n{body}\n"),
    );
}

/// Claude 用户级 MCP 可能落在 `$HOME/.claude.json` 或 `$CLAUDE_CONFIG_DIR/.claude.json`。
fn write_claude_mcp(home: &FsPath, body: &str) {
    write(&home.join(".claude.json"), body);
    write(&home.join(".claude/.claude.json"), body);
}

fn active_claude_mcp_path() -> PathBuf {
    claude_user_mcp_config_path(&TargetEnvironment::from_process())
}

fn assert_active_mcp_lacks_key(key: &str) {
    let path = active_claude_mcp_path();
    let text = fs::read_to_string(&path).unwrap_or_default();
    assert!(
        !text.contains(key),
        "{} still contains {key}: {text}",
        path.display()
    );
}

fn assert_active_mcp_contains(needle: &str) {
    let path = active_claude_mcp_path();
    let text = fs::read_to_string(&path).unwrap_or_default();
    assert!(
        text.contains(needle),
        "{} missing {needle}: {text}",
        path.display()
    );
}

fn assert_dest_sees_mcp(
    dest_home: &FsPath,
    dest_claude: &app_lib::agent_hub::user_mirror::UserMirrorAgentInventoryDto,
    native_id: &str,
) {
    let env = TargetEnvironment::from_process();
    let mcp_path = claude_user_mcp_config_path(&env);
    let mcp_text = fs::read_to_string(&mcp_path).unwrap_or_else(|_| {
        format!(
            "MISSING file {} exists_home_json={} exists_config_json={}",
            mcp_path.display(),
            dest_home.join(".claude.json").is_file(),
            dest_home.join(".claude/.claude.json").is_file()
        )
    });
    assert!(
        dest_claude
            .items
            .iter()
            .any(|item| item.kind == PortableAssetKind::Mcp && item.native_id == native_id),
        "dest inventory must see {native_id} MCP before apply; home={:?} HOME={:?} CLAUDE_CONFIG_DIR={:?} mcp_path={} file={mcp_text} items={:?}",
        env.home,
        std::env::var("HOME"),
        std::env::var("CLAUDE_CONFIG_DIR"),
        mcp_path.display(),
        dest_claude.items
    );
}

/// SAFETY: 本文件必须 `--test-threads=1`；切换 owner 会改进程 HOME / CC_PARTNER_DATA_DIR。
///
/// DualEnv 单测用空 vars 解析 MCP 为 `$HOME/.claude.json`。这里清掉 CLAUDE_CONFIG_DIR
/// 等覆盖键，避免 MCP 落到 `$HOME/.claude/.claude.json` 而夹具写不到。
fn activate_owner(data_dir: &FsPath, home: &FsPath, device_id: &str) {
    std::env::set_var("CC_PARTNER_DATA_DIR", data_dir);
    std::env::set_var("HOME", home);
    for key in [
        "CLAUDE_CONFIG_DIR",
        "CODEX_HOME",
        "OPENCODE_CONFIG_DIR",
        "OPENCODE_CONFIG",
        "GROK_HOME",
        "GEMINI_HOME",
        "CURSOR_HOME",
        "XDG_CONFIG_HOME",
    ] {
        std::env::remove_var(key);
    }
    write(
        &data_dir.join("config.json"),
        &format!(
            r#"{{
  "device_id": "{device_id}",
  "device_name": "{device_id}",
  "http_port": 0,
  "receive_dir": "{receive}",
  "db_path": "{db}",
  "screenshot_hotkey": "<cmd>+s",
  "prompt_optimizer_hotkey": "<ctrl>",
  "prompt_optimizer_fill_language": "zh"
}}"#,
            receive = data_dir.join("received").display(),
            db = data_dir.join("data.db").display(),
        ),
    );
}

fn seed_source_home(home: &FsPath) {
    write(&home.join(".claude/CLAUDE.md"), SRC_CLAUDE);
    write(&home.join(".codex/AGENTS.md"), SRC_CODEX);
    write(&home.join(".grok/AGENTS.md"), SRC_GROK);
    write_skill(&home.join(".claude/skills"), "keep", KEEP_SKILL_BODY);
    write_claude_mcp(
        home,
        &format!(
            r#"{{"mcpServers":{{"src-api":{{"command":"uvx","args":["srv"],"env":{{"API_TOKEN":"{CREDENTIAL}"}},"enabled":true}}}}}}"#
        ),
    );
}

fn seed_dest_home(home: &FsPath) {
    write(&home.join(".claude/CLAUDE.md"), "OLD-DEST");
    write(&home.join(".codex/AGENTS.md"), "OLD-CODEX");
    write(&home.join(".grok/AGENTS.md"), "OLD-GROK");
    write_skill(
        &home.join(".claude/skills"),
        "dest-skill",
        "DEST-ONLY-SKILL-BODY",
    );
    write_skill(&home.join(".claude/skills"), "keep", "OLD-KEEP");
    write_claude_mcp(
        home,
        r#"{"mcpServers":{"dest-mcp":{"command":"uvx","args":["gone"],"env":{"TOKEN":"dest-only-secret-beta"},"enabled":true}}}"#,
    );
    write(&home.join("proj-not-config/AGENTS.md"), REPO_AGENTS);
}

fn seed_dest_extras_plugin_and_agents(home: &FsPath) {
    write(
        &home.join(".claude/plugins/dest-plugin/.claude-plugin/plugin.json"),
        r#"{"name":"dest-plugin","version":"1.0.0"}"#,
    );
    write(
        &home.join(".claude/settings.json"),
        r#"{"enabledPlugins":{"dest-plugin":true}}"#,
    );
    write_skill(
        &home.join(".agents/skills"),
        "agents-fixture",
        "AGENTS-FIXTURE",
    );
}

fn persist_plan_record(plan: &UserMirrorPlanDto) -> UserMirrorPlanRecord {
    UserMirrorPlanRecord {
        plan_token: plan.plan_token.clone(),
        expires_at: plan.expires_at.clone(),
        plan_json: serde_json::to_string(plan).expect("plan json"),
        client_request_id: None,
        claimed_at: None,
        consumed_at: None,
        result_json: None,
        created_at: plan.expires_at.clone(),
    }
}

fn claude_agent<'a>(
    inventory: &'a UserMirrorInventoryDto,
) -> &'a app_lib::agent_hub::user_mirror::UserMirrorAgentInventoryDto {
    inventory
        .agents
        .iter()
        .find(|agent| agent.target == AgentTarget::Claude)
        .expect("claude agent")
}

fn assert_all_hub_targets(inventory: &UserMirrorInventoryDto) {
    let present: Vec<_> = inventory.agents.iter().map(|agent| agent.target).collect();
    for target in all_hub_targets() {
        assert!(
            present.contains(&target),
            "inventory missing hub target {target:?}: {present:?}"
        );
    }
}

fn assert_mcp_dto_has_no_secret(json: &str, home: &FsPath) {
    let lower = json.to_ascii_lowercase();
    assert!(!json.contains(CREDENTIAL), "MCP DTO leaked secret: {json}");
    assert!(
        !json.contains("dest-only-secret-beta"),
        "MCP DTO leaked dest secret: {json}"
    );
    assert!(
        !lower.contains("sourcepath"),
        "inventory must be metadata-only (no sourcePath): {json}"
    );
    let home_s = home.to_string_lossy();
    assert!(
        !json.contains(home_s.as_ref()),
        "inventory must not leak HOME path {home_s}: {json}"
    );
}

fn native_hash<'a>(
    inventory: &'a UserMirrorInventoryDto,
    target: AgentTarget,
    logical_id: &str,
) -> Option<&'a str> {
    inventory
        .agents
        .iter()
        .find(|agent| agent.target == target)
        .and_then(|agent| {
            agent
                .native_files
                .iter()
                .find(|file| file.logical_id == logical_id)
                .and_then(|file| file.content_hash.as_deref())
        })
}

#[derive(Clone)]
struct CapablePeer {
    inventory: UserMirrorInventoryDto,
    selection: Arc<AsyncMutex<Option<UserMirrorSelectionResponse>>>,
    hits: Arc<AtomicUsize>,
}

#[derive(Deserialize)]
struct ObjectQuery {
    #[serde(default)]
    offset: u64,
}

async fn capable_health(State(peer): State<CapablePeer>) -> Json<serde_json::Value> {
    peer.hits.fetch_add(1, Ordering::SeqCst);
    let info = server_protocol_info();
    Json(json!({
        "ok": true,
        "device_id": SOURCE_DEVICE,
        "device_name": "User Mirror Source",
        "http_port": 0,
        "ts": 1,
        "protocol_version": info.protocol_version,
        "capabilities": info.capabilities,
    }))
}

async fn capable_inventory(
    State(peer): State<CapablePeer>,
    Json(_body): Json<serde_json::Value>,
) -> Json<UserMirrorInventoryDto> {
    peer.hits.fetch_add(1, Ordering::SeqCst);
    Json(peer.inventory.clone())
}

async fn capable_selection(
    State(peer): State<CapablePeer>,
    Json(_body): Json<UserMirrorSelectionQuery>,
) -> Result<Json<UserMirrorSelectionResponse>, axum::http::StatusCode> {
    peer.hits.fetch_add(1, Ordering::SeqCst);
    let guard = peer.selection.lock().await;
    guard
        .clone()
        .map(Json)
        .ok_or(axum::http::StatusCode::NOT_FOUND)
}

async fn capable_object(
    State(peer): State<CapablePeer>,
    Path((transfer_id, object_hash)): Path<(String, String)>,
    Query(query): Query<ObjectQuery>,
) -> Result<Vec<u8>, axum::http::StatusCode> {
    peer.hits.fetch_add(1, Ordering::SeqCst);
    source_read_user_mirror_object_chunk(&transfer_id, &object_hash, query.offset)
        .map_err(|_| axum::http::StatusCode::NOT_FOUND)
}

async fn spawn_capable_peer(peer: CapablePeer) -> (String, tokio::task::JoinHandle<()>) {
    let app = Router::new()
        .route("/api/health", get(capable_health))
        .route(
            "/api/agent-hub/user-mirror/inventory",
            post(capable_inventory),
        )
        .route(
            "/api/agent-hub/user-mirror/selection",
            post(capable_selection),
        )
        .route(
            "/api/agent-hub/user-mirror/objects/:transferId/:objectHash",
            get(capable_object),
        )
        .with_state(peer);
    bind_loopback(app).await
}

#[derive(Clone)]
struct MissingPeer {
    portable_hits: Arc<AtomicUsize>,
    old_push_hits: Arc<AtomicUsize>,
}

async fn missing_health(State(_peer): State<MissingPeer>) -> Json<serde_json::Value> {
    Json(json!({
        "ok": true,
        "device_id": "user-mirror-old-peer",
        "device_name": "Old Peer",
        "http_port": 0,
        "ts": 1,
        "protocol_version": 1,
        "capabilities": [
            "errors.envelope.v1",
            CAPABILITY_PORTABLE_PULL_V1,
            CAPABILITY_AGENT_HUB_V1,
        ],
    }))
}

async fn hit_portable(State(peer): State<MissingPeer>) -> axum::http::StatusCode {
    peer.portable_hits.fetch_add(1, Ordering::SeqCst);
    axum::http::StatusCode::NOT_FOUND
}

async fn hit_old_push(State(peer): State<MissingPeer>) -> axum::http::StatusCode {
    peer.old_push_hits.fetch_add(1, Ordering::SeqCst);
    axum::http::StatusCode::NOT_FOUND
}

async fn spawn_missing_peer(peer: MissingPeer) -> (String, tokio::task::JoinHandle<()>) {
    let app = Router::new()
        .route("/api/health", get(missing_health))
        .route("/api/agent-hub/portable/inventory", post(hit_portable))
        .route("/api/agent-hub/portable/selection", post(hit_portable))
        .route(
            "/api/agent-hub/portable/objects/:transferId/:objectHash",
            get(hit_portable),
        )
        .route(
            "/api/agent-hub/portable/transfers/:transferId/release",
            post(hit_portable),
        )
        .route("/api/agent-hub/push/prepare", post(hit_old_push))
        .route(
            "/api/agent-hub/push/:transferId/objects/:objectHash",
            put(hit_old_push),
        )
        .route("/api/agent-hub/push/:transferId/commit", post(hit_old_push))
        .with_state(peer);
    bind_loopback(app).await
}

async fn bind_loopback(app: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    (format!("http://{addr}"), handle)
}

/// L2-AGENT-HUB-USER-MIRROR-001：双 data_dir 全量镜像 + frozen loopback 能力门。
#[tokio::test]
async fn l2_agent_hub_user_mirror_001_full_mirror() {
    let dual = setup_dual_env();
    seed_source_home(&dual.source_home);
    seed_dest_home(&dual.dest_home);

    activate_owner(&dual.source_data, &dual.source_home, SOURCE_DEVICE);
    invalidate_portable_inventory_cache();
    let source_state = build_app_state(Arc::new(HeadlessBackendUi::new(dual.source_data.clone())))
        .await
        .expect("source state");
    let source_inventory = build_local_user_mirror_inventory(&source_state, SOURCE_DEVICE)
        .await
        .expect("source inventory");
    assert_all_hub_targets(&source_inventory);
    let source_json = serde_json::to_string(&source_inventory).unwrap();
    assert_mcp_dto_has_no_secret(&source_json, &dual.source_home);
    let source_claude = claude_agent(&source_inventory);
    assert!(
        source_claude
            .native_files
            .iter()
            .any(|file| file.logical_id == "claude.native.CLAUDE.md" && file.exists),
        "source Claude CLAUDE.md missing: {:?}",
        source_claude.native_files
    );
    assert!(
        source_claude
            .items
            .iter()
            .any(|item| item.kind == PortableAssetKind::Skill && item.native_id == "keep"),
        "source keep skill missing: {:?}",
        source_claude.items
    );
    assert!(
        source_inventory.credential_bearing_count > 0,
        "source MCP TOKEN must count as credential-bearing"
    );

    let built = freeze_user_mirror_selection(&source_state, &source_inventory)
        .await
        .expect("freeze");
    let missing_object_hashes: Vec<String> = built
        .envelope
        .objects
        .iter()
        .map(|object| object.hash.clone())
        .collect();
    let selection = UserMirrorSelectionResponse {
        transfer_id: built.transfer_id.clone(),
        envelope: built.envelope.clone(),
        item_bindings: built.item_bindings.clone(),
        missing_object_hashes,
    };
    if let Some(object) = built.envelope.objects.first() {
        let chunk = source_read_user_mirror_object_chunk(&built.transfer_id, &object.hash, 0)
            .expect("chunk");
        assert!(chunk.len() <= 8 * 1024 * 1024);
    }

    let capable = CapablePeer {
        inventory: source_inventory.clone(),
        selection: Arc::new(AsyncMutex::new(Some(selection))),
        hits: Arc::new(AtomicUsize::new(0)),
    };
    let (capable_url, capable_handle) = spawn_capable_peer(capable.clone()).await;
    let client = reqwest::Client::new();
    let health: serde_json::Value = client
        .get(format!("{capable_url}/api/health"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let caps = health["capabilities"].as_array().expect("caps");
    assert!(
        caps.iter()
            .any(|cap| cap.as_str() == Some(CAPABILITY_USER_MIRROR_V1)),
        "health must declare {CAPABILITY_USER_MIRROR_V1}: {caps:?}"
    );
    PeerClient::new()
        .require_capability(&capable_url, CAPABILITY_USER_MIRROR_V1)
        .await
        .expect("capable peer supports user-mirror");
    let peer_inv: UserMirrorInventoryDto = client
        .post(format!("{capable_url}/api/agent-hub/user-mirror/inventory"))
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        peer_inv.inventory_snapshot_hash,
        source_inventory.inventory_snapshot_hash
    );
    assert_mcp_dto_has_no_secret(
        &serde_json::to_string(&peer_inv).unwrap(),
        &dual.source_home,
    );

    let missing = MissingPeer {
        portable_hits: Arc::new(AtomicUsize::new(0)),
        old_push_hits: Arc::new(AtomicUsize::new(0)),
    };
    let (missing_url, missing_handle) = spawn_missing_peer(missing.clone()).await;
    let unsupported = PeerClient::new()
        .require_capability(&missing_url, CAPABILITY_USER_MIRROR_V1)
        .await;
    match unsupported {
        Err(PeerCallError::Unsupported { capability, .. }) => {
            assert_eq!(capability, CAPABILITY_USER_MIRROR_V1);
        }
        other => panic!("{USER_MIRROR_CAPABILITY_UNSUPPORTED} expected, got {other:?}"),
    }
    assert_eq!(
        missing.portable_hits.load(Ordering::SeqCst),
        0,
        "missing-capability peer must receive zero portable-pull hits"
    );
    assert_eq!(
        missing.old_push_hits.load(Ordering::SeqCst),
        0,
        "missing-capability peer must receive zero old agent-hub/push hits"
    );

    activate_owner(&dual.dest_data, &dual.dest_home, DEST_DEVICE);
    invalidate_portable_inventory_cache();
    let dest_state = build_app_state(Arc::new(HeadlessBackendUi::new(dual.dest_data.clone())))
        .await
        .expect("dest state");
    let dest_inventory = build_local_user_mirror_inventory(&dest_state, DEST_DEVICE)
        .await
        .expect("dest inventory");
    let dest_claude_before = claude_agent(&dest_inventory);
    assert_dest_sees_mcp(&dual.dest_home, dest_claude_before, "dest-mcp");
    let plan = preview_from_two_inventories(
        &source_inventory,
        &dest_inventory,
        SOURCE_DEVICE,
        DEST_DEVICE,
        UserMirrorDirection::Pull,
    );
    let claude_plan = plan
        .agents
        .iter()
        .find(|agent| agent.target == AgentTarget::Claude)
        .expect("claude plan");
    assert!(
        claude_plan
            .mcp_deletes
            .iter()
            .any(|change| change.native_id == "dest-mcp"),
        "preview must delete dest-mcp: {claude_plan:?}"
    );
    dest_state
        .agent_hub_repo
        .insert_user_mirror_plan(persist_plan_record(&plan))
        .await
        .expect("persist plan");
    let result = apply_user_mirror(
        &dest_state,
        ApplyUserMirrorRequest {
            plan_token: plan.plan_token.clone(),
            client_request_id: "req-full-001".into(),
        },
        &built.object_bytes,
        &built.item_bindings,
    )
    .await
    .expect("apply");
    let result_json = serde_json::to_string(&result).unwrap();
    assert_mcp_dto_has_no_secret(&result_json, &dual.dest_home);
    assert!(
        !result.partial,
        "full mirror must not be partial: {result:?}"
    );
    assert_eq!(
        fs::read_to_string(dual.dest_home.join(".claude/CLAUDE.md")).unwrap(),
        SRC_CLAUDE
    );
    let keep_body =
        fs::read_to_string(dual.dest_home.join(".claude/skills/keep/SKILL.md")).unwrap_or_default();
    assert!(
        keep_body.contains(KEEP_SKILL_BODY),
        "dest keep skill must align with source, got {keep_body:?}"
    );
    assert_eq!(
        fs::read_to_string(dual.dest_home.join("proj-not-config/AGENTS.md")).unwrap(),
        REPO_AGENTS,
        "Grok must not rewrite the repo AGENTS.md fixture"
    );
    assert!(
        !fs::read_to_string(dual.dest_home.join("proj-not-config/AGENTS.md"))
            .unwrap()
            .contains(SRC_GROK),
        "repo AGENTS.md must not contain Grok source body"
    );
    assert_eq!(
        fs::read_to_string(dual.dest_home.join(".grok/AGENTS.md")).unwrap(),
        SRC_GROK
    );

    invalidate_portable_inventory_cache();
    let dest_after = build_local_user_mirror_inventory(&dest_state, DEST_DEVICE)
        .await
        .expect("dest rescan");
    let dest_claude = claude_agent(&dest_after);
    let source_claude = claude_agent(&source_inventory);
    assert_eq!(dest_claude.slots.common, source_claude.slots.common);
    assert_eq!(dest_claude.slots.adapted, source_claude.slots.adapted);
    assert_eq!(dest_claude.slots.exclusive, source_claude.slots.exclusive);
    assert_eq!(
        native_hash(&dest_after, AgentTarget::Claude, "claude.native.CLAUDE.md"),
        native_hash(
            &source_inventory,
            AgentTarget::Claude,
            "claude.native.CLAUDE.md"
        )
    );
    assert!(
        dest_claude
            .items
            .iter()
            .any(|item| item.kind == PortableAssetKind::Skill && item.native_id == "keep"),
        "keep skill must remain on dest: {:?}",
        dest_claude.items
    );
    assert!(
        !dest_claude
            .items
            .iter()
            .any(|item| item.kind == PortableAssetKind::Skill && item.native_id == "dest-skill"),
        "dest-skill must be gone: {:?}",
        dest_claude.items
    );
    assert!(
        !dest_claude
            .items
            .iter()
            .any(|item| item.kind == PortableAssetKind::Mcp && item.native_id == "dest-mcp"),
        "dest-mcp must be gone: {:?}",
        dest_claude.items
    );
    assert_active_mcp_lacks_key("dest-mcp");
    assert_active_mcp_contains(CREDENTIAL);
    assert!(capable.hits.load(Ordering::SeqCst) > 0);

    capable_handle.abort();
    missing_handle.abort();
    assert_eq!(EVIDENCE_001, "L2-AGENT-HUB-USER-MIRROR-001");
    println!("{EVIDENCE_001}: full user-mirror DualEnv + frozen capability gate certified");
}

/// L2-AGENT-HUB-USER-MIRROR-002：单 Agent 写失败不回滚，同 request 重放。
#[tokio::test]
async fn l2_agent_hub_user_mirror_002_partial_no_rollback() {
    let dual = setup_dual_env();
    write(&dual.source_home.join(".claude/CLAUDE.md"), SRC_CLAUDE);
    write(&dual.source_home.join(".codex/AGENTS.md"), SRC_CODEX);
    write(&dual.dest_home.join(".claude/CLAUDE.md"), "OLD-DEST");
    write(&dual.dest_home.join(".codex/AGENTS.md"), "OLD-CODEX");

    activate_owner(&dual.source_data, &dual.source_home, SOURCE_DEVICE);
    invalidate_portable_inventory_cache();
    let source_state = build_app_state(Arc::new(HeadlessBackendUi::new(dual.source_data.clone())))
        .await
        .expect("source state");
    let source_inventory = build_local_user_mirror_inventory(&source_state, SOURCE_DEVICE)
        .await
        .expect("source inventory");
    let built = freeze_user_mirror_selection(&source_state, &source_inventory)
        .await
        .expect("freeze");

    activate_owner(&dual.dest_data, &dual.dest_home, DEST_DEVICE);
    invalidate_portable_inventory_cache();
    let dest_state = build_app_state(Arc::new(HeadlessBackendUi::new(dual.dest_data.clone())))
        .await
        .expect("dest state");
    let dest_inventory = build_local_user_mirror_inventory(&dest_state, DEST_DEVICE)
        .await
        .expect("dest inventory");
    let plan = preview_from_two_inventories(
        &source_inventory,
        &dest_inventory,
        SOURCE_DEVICE,
        DEST_DEVICE,
        UserMirrorDirection::Pull,
    );
    dest_state
        .agent_hub_repo
        .insert_user_mirror_plan(persist_plan_record(&plan))
        .await
        .expect("persist plan");

    let codex_dir = dual.dest_home.join(".codex");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&codex_dir).expect("codex meta").permissions();
        perms.set_mode(0o555);
        fs::set_permissions(&codex_dir, perms).expect("codex readonly");
    }

    let request = ApplyUserMirrorRequest {
        plan_token: plan.plan_token.clone(),
        client_request_id: "req-partial-002".into(),
    };
    let first = apply_user_mirror(
        &dest_state,
        request.clone(),
        &built.object_bytes,
        &built.item_bindings,
    )
    .await
    .expect("apply partial");
    assert!(
        first.partial,
        "one Agent write failure must set partial=true: {first:?}"
    );
    let claude = first
        .agents
        .iter()
        .find(|agent| agent.target == AgentTarget::Claude)
        .expect("claude result");
    assert_eq!(claude.state, UserMirrorItemState::Succeeded, "{claude:?}");
    let codex = first
        .agents
        .iter()
        .find(|agent| agent.target == AgentTarget::Codex)
        .expect("codex result");
    assert_eq!(codex.state, UserMirrorItemState::Failed, "{codex:?}");
    assert_eq!(
        fs::read_to_string(dual.dest_home.join(".claude/CLAUDE.md")).unwrap(),
        SRC_CLAUDE,
        "successful Agent files must keep source content (no rollback)"
    );
    assert_eq!(
        fs::read_to_string(dual.dest_home.join(".codex/AGENTS.md")).unwrap(),
        "OLD-CODEX",
        "failed Agent dest file must not be overwritten"
    );

    let replay = apply_user_mirror(&dest_state, request, &Default::default(), &[])
        .await
        .expect("replay");
    assert_eq!(
        serde_json::to_string(&replay).unwrap(),
        serde_json::to_string(&first).unwrap(),
        "same clientRequestId must replay the same result"
    );
    let got: UserMirrorResultDto = get_user_mirror(&dest_state, "req-partial-002")
        .await
        .expect("get");
    assert_eq!(got.partial, first.partial);
    assert_eq!(got.client_request_id, first.client_request_id);
    assert_eq!(
        serde_json::to_string(&got.agents).unwrap(),
        serde_json::to_string(&first.agents).unwrap()
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&codex_dir).expect("codex meta").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&codex_dir, perms).expect("codex restore");
    }

    assert_eq!(EVIDENCE_002, "L2-AGENT-HUB-USER-MIRROR-002");
    println!("{EVIDENCE_002}: partial no-rollback + request replay certified");
}

/// L2-AGENT-HUB-USER-MIRROR-003：dest extras 策略。
#[tokio::test]
async fn l2_agent_hub_user_mirror_003_extras_policy() {
    let dual = setup_dual_env();
    seed_source_home(&dual.source_home);
    seed_dest_home(&dual.dest_home);
    seed_dest_extras_plugin_and_agents(&dual.dest_home);
    let agents_fixture = dual
        .dest_home
        .join(".agents/skills/agents-fixture/SKILL.md");
    let plugin_root = dual.dest_home.join(".claude/plugins/dest-plugin");

    activate_owner(&dual.source_data, &dual.source_home, SOURCE_DEVICE);
    invalidate_portable_inventory_cache();
    let source_state = build_app_state(Arc::new(HeadlessBackendUi::new(dual.source_data.clone())))
        .await
        .expect("source state");
    let source_inventory = build_local_user_mirror_inventory(&source_state, SOURCE_DEVICE)
        .await
        .expect("source inventory");
    let built = freeze_user_mirror_selection(&source_state, &source_inventory)
        .await
        .expect("freeze");

    activate_owner(&dual.dest_data, &dual.dest_home, DEST_DEVICE);
    invalidate_portable_inventory_cache();
    let dest_state = build_app_state(Arc::new(HeadlessBackendUi::new(dual.dest_data.clone())))
        .await
        .expect("dest state");
    let dest_inventory = build_local_user_mirror_inventory(&dest_state, DEST_DEVICE)
        .await
        .expect("dest inventory");
    let dest_claude_before = claude_agent(&dest_inventory);
    assert_dest_sees_mcp(&dual.dest_home, dest_claude_before, "dest-mcp");
    let plan = preview_from_two_inventories(
        &source_inventory,
        &dest_inventory,
        SOURCE_DEVICE,
        DEST_DEVICE,
        UserMirrorDirection::Pull,
    );
    let claude_plan = plan
        .agents
        .iter()
        .find(|agent| agent.target == AgentTarget::Claude)
        .expect("claude plan");
    assert!(
        claude_plan
            .mcp_deletes
            .iter()
            .any(|change| change.native_id == "dest-mcp"),
        "preview must delete dest-mcp: {claude_plan:?}"
    );
    dest_state
        .agent_hub_repo
        .insert_user_mirror_plan(persist_plan_record(&plan))
        .await
        .expect("persist plan");
    let result = apply_user_mirror(
        &dest_state,
        ApplyUserMirrorRequest {
            plan_token: plan.plan_token.clone(),
            client_request_id: "req-extras-003".into(),
        },
        &built.object_bytes,
        &built.item_bindings,
    )
    .await
    .expect("apply extras");
    let claude = result
        .agents
        .iter()
        .find(|agent| agent.target == AgentTarget::Claude)
        .expect("claude");
    assert_eq!(claude.state, UserMirrorItemState::Succeeded, "{result:?}");

    invalidate_portable_inventory_cache();
    let dest_after = build_local_user_mirror_inventory(&dest_state, DEST_DEVICE)
        .await
        .expect("dest rescan");
    let dest_claude = claude_agent(&dest_after);
    assert!(
        dest_claude
            .items
            .iter()
            .any(|item| item.kind == PortableAssetKind::Skill && item.native_id == "keep"),
        "keep skill must be attached: {:?}",
        dest_claude.items
    );
    assert!(
        !dest_claude
            .items
            .iter()
            .any(|item| item.kind == PortableAssetKind::Skill && item.native_id == "dest-skill"),
        "dest-skill must be detached from viewing inventory: {:?}",
        dest_claude.items
    );
    assert!(
        agents_fixture.is_file(),
        "~/.agents fixture must remain after skill detach"
    );
    assert!(
        fs::read_to_string(&agents_fixture)
            .unwrap()
            .contains("AGENTS-FIXTURE"),
        "~/.agents source tree must not be destroyed"
    );

    assert!(
        plugin_root.join(".claude-plugin/plugin.json").is_file(),
        "plugin package dir must remain after disable (not uninstall)"
    );
    let dest_only_plugin = dest_claude.items.iter().find(|item| {
        item.kind == PortableAssetKind::Plugin
            && (item.native_id == "dest-plugin" || item.native_id.starts_with("dest-plugin"))
    });
    let plugin = dest_only_plugin.expect("dest-plugin still inventoried after disable");
    assert_eq!(
        plugin.actual_enabled,
        Some(false),
        "dest-plugin must be viewing-disabled: {plugin:?}"
    );

    assert_active_mcp_lacks_key("dest-mcp");
    assert_active_mcp_contains("src-api");
    assert!(
        !dest_claude
            .items
            .iter()
            .any(|item| item.kind == PortableAssetKind::Mcp && item.native_id == "dest-mcp"),
        "dest-mcp must be gone from inventory: {:?}",
        dest_claude.items
    );

    assert_eq!(EVIDENCE_003, "L2-AGENT-HUB-USER-MIRROR-003");
    println!("{EVIDENCE_003}: skill detach / plugin disable / MCP delete certified");
}
