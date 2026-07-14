# Backend Operations

Operator-facing guide for the headless binary **`cc-partner-backend`**: lifecycle, ports, firewall/mDNS checks, data/control/log paths, and `doctor`. Engineering invariants (locks, probe timeouts, GUI advertise rules) stay in [`src-tauri/CLAUDE.md`](../../src-tauri/CLAUDE.md).

## Fixed LAN trust boundary

cc-partner is local/LAN only. There is a single fixed LAN behavior for all business APIs (P2P, Mobile, Workbench, Orchestrator):

- Loopback and supported LAN socket peers are fully allowed **without** accounts, pairing, tokens, cookies, sessions, signatures, or device identity.
- Socket peer IP comes from the real TCP `ConnectInfo` only (never `Forwarded` / `X-Forwarded-For` / `X-Real-IP`).
- Supported ranges: IPv4 loopback `127.0.0.0/8`, RFC1918, IPv4 link-local `169.254.0.0/16`, IPv6 loopback `::1`, IPv6 ULA `fc00::/7`, IPv6 link-local `fe80::/10`. IPv4-mapped IPv6 is normalized to IPv4 before classification. Other peers get 403 before the handler.
- Listener may bind wildcard `0.0.0.0:<actualPort>`; the LAN-only enforcement is the socket gate, not “bind only LAN interfaces”.
- Host/Origin/Content-Type guards reduce browser CSRF / DNS rebinding risk. Native peers may omit Origin. Ordinary `/api/*` rejects `Origin: null`; opaque null Origin is allowed only for a live Browser Preview session path after registry lookup.
- Resource limits (absolute caps, not a per-route auth matrix): global body **32 MiB**, transfer chunk **960 KiB**, Workbench text save **5 MiB**, preview proxy body **32 MiB**.
- `POST /api/backend/control/stop` is a **local lifecycle** control: loopback peer **and** the control-file token. The token must not appear in business APIs, health, mDNS, UI, doctor, or logs.
- Old and new native peers continue credential-free. There is **no** LAN permission capability negotiation and **no** configurable LAN exposure/read-only product mode.

**Remaining risk (fixed):** Any device on the same reachable network can read, write, and execute; the system does not verify caller identity.

Chinese product wording (must stay equivalent): 同一可达网络中的任何设备均可读取、写入和执行；系统不验证调用者身份。

## Lifecycle

Usage:

```text
cc-partner-backend <start|serve|stop|status|doctor [--json]>
```

| Subcommand | Role | Success exit | Failure exit |
| --- | --- | --- | --- |
| `start` | Detach `serve` child; adopt existing Running instance after reaping own child if needed | 0 | 1 |
| `serve` | Foreground runtime (internal; used by `start`) — advertise + browse | 0 | 1 |
| `stop` | Local control stop + wait for process exit | 0 | 1 |
| `status` | Machine-readable JSON `{kind, control?:{pid,port}, error?}` — **no control token** | 0 | 1 |
| `doctor` | Human-readable health report on stdout | 0 / 1 / 2 | 2 on parse/collect failure |
| `doctor --json` | **stdout pure single-line** `DoctorSnapshot` JSON; tracing/errors on stderr | 0 / 1 / 2 | 2 on parse/collect failure |
| unknown / bad args | Usage on stderr | — | **2** |

Lifecycle commands (`start` / `serve` / `stop` / `status`) stay **0/1**. Only `doctor` uses the **0/1/2** overall map.

### Packaged / PATH

```text
cc-partner-backend start
cc-partner-backend status
cc-partner-backend doctor
cc-partner-backend doctor --json
cc-partner-backend stop
```

### Development (cargo)

```bash
cargo run --locked --bin cc-partner-backend -- start
cargo run --locked --bin cc-partner-backend -- status
cargo run --locked --bin cc-partner-backend -- doctor
cargo run --locked --bin cc-partner-backend -- doctor --json
cargo run --locked --bin cc-partner-backend -- stop
```

`serve` is not a normal operator entry; prefer `start` / `stop`.

## Ports & discovery

| Role | Protocol | Value |
| --- | --- | --- |
| Preferred P2P HTTP | TCP | **62116** (`DEFAULT_HTTP_PORT`) |
| Actual listen port | TCP | Preferred bind; on `AddrInUse` **increment by 1** until success → stored as actual |
| Config `http_port` `0` / invalid | — | Means “use preferred default”, **not** OS ephemeral `port=0` bind |
| Health field | `GET /api/health` → `http_port` | **Actual** listening port |
| Device discovery | UDP | **5353** (mDNS, service `_cc-partner._tcp.local.`) |

Desktop UI talks to Rust via Tauri `invoke` only — **no** local HTTP API port for the desktop frontend. Mobile SPA and peer P2P share the actual HTTP port.

Discover the actual port:

```bash
curl -sS "http://127.0.0.1:62116/api/health"
# or the port printed by status / doctor / mobile access URL
```

Example health body (snake_case):

```json
{
  "ok": true,
  "device_id": "...",
  "device_name": "...",
  "http_port": 62116,
  "ts": 0,
  "protocol_version": 1,
  "capabilities": ["attention.v1", "errors.envelope.v1", "orchestrator.runtime-snapshot.v1"]
}
```

## Firewall & mDNS checks

The app **does not** change host firewall rules. If peers appear in discovery but transfer / Mobile Workbench / remote projects fail, confirm same LAN and **manually** allow inbound rules, preferably limited to a Private/Home/LAN profile:

| Purpose | Rule |
| --- | --- |
| Discovery | UDP **5353** inbound |
| P2P HTTP / Mobile / Workbench | TCP **actual** port (preferred **62116**, increment when occupied; use `http_port` from health/status/doctor) |

Opening these ports means **any device on the reachable network** can call business APIs without credentials. That is intentional product semantics, not an “authenticated peer” guarantee.

**macOS** (app-based firewall example):

```bash
sudo /usr/libexec/ApplicationFirewall/socketfilterfw --add /Applications/cc-partner.app
sudo /usr/libexec/ApplicationFirewall/socketfilterfw --unblockapp /Applications/cc-partner.app
```

**Windows** (admin PowerShell; replace port with actual `http_port` if needed):

```powershell
New-NetFirewallRule -DisplayName "cc-partner P2P HTTP" -Direction Inbound -Action Allow -Protocol TCP -LocalPort 62116
New-NetFirewallRule -DisplayName "cc-partner mDNS" -Direction Inbound -Action Allow -Protocol UDP -LocalPort 5353
```

**Ubuntu / Linux (ufw)**:

```bash
sudo ufw allow 62116/tcp comment 'cc-partner P2P HTTP'
sudo ufw allow 5353/udp comment 'cc-partner mDNS'
sudo ufw reload
```

Verify:

```bash
curl -sS "http://<peer-ip>:<actual-port>/api/health"
```

VPN and guest Wi‑Fi often break mDNS even when TCP works.

## Paths (control / data / logs)

Default data root: `<HOME>/.cc-partner`. Override with an **absolute** `CC_PARTNER_DATA_DIR` (blank / relative / NUL rejected). All control, DB, and log paths hang off that root.

| Item | Default path (home notation) |
| --- | --- |
| Data root | `<HOME>/.cc-partner` |
| Config | `<HOME>/.cc-partner/config.json` |
| SQLite | `<HOME>/.cc-partner/data.db` |
| Control JSON | `<HOME>/.cc-partner/backend-control.json` |
| PID file | `<HOME>/.cc-partner/backend.pid` |
| Start lock | `<HOME>/.cc-partner/backend-start.lock` |
| Serve lock | `<HOME>/.cc-partner/backend-serve.lock` |
| Current log | `<HOME>/.cc-partner/logs/backend.log` |
| Log history | `<HOME>/.cc-partner/logs/backend.log.1` … `.3` (`.1` newest) |
| Default receive dir (files) | `<HOME>/cc-partner-files` (no home + data-dir override → `<data_dir>/received-files`) |

`status` / `doctor` never print the control **token**. Stop uses the local control route with the token from the control file only on the same machine.

### Log rotation & privacy

| Policy | Value |
| --- | --- |
| Current max size | **5 MiB** |
| History files | **3** (`.1` … `.3`); **never** `.4` |
| Upload / telemetry | **None** — logs stay on disk; doctor does not upload |
| Sanitization | Home → `<HOME>`; secrets / tokens / Prompt bodies redacted in diagnostics |

Only the `serve` process writes `backend.log`. `doctor` uses stderr tracing only and does **not** open the log file for write (read-only tail for recent errors).

## Doctor

### Exit status

| Overall `status` | Exit code | Typical meaning |
| --- | --- | --- |
| `healthy` | **0** | Core paths OK; no warnings elevating status |
| `degraded` | **1** | Optional deps missing, recoverable stale control, mDNS warning, malformed log lines, etc. |
| `unhealthy` | **2** | Core data/db/log path error, backend health error, or doctor collect/parse failure |

**Stopped** backend is normally **healthy / info** (exit 0), not an error. Missing optional tools (tmux, WSL, Claude CLI, …) usually yield **degraded / 1**.

### Human output

```bash
cc-partner-backend doctor
# or
cargo run --locked --bin cc-partner-backend -- doctor
```

Expect a status line, check table (warnings/errors), control/log paths already using `<HOME>` where applicable, and dependency notes. Treat exit code as the machine contract; text is for operators.

### JSON output

```bash
cc-partner-backend doctor --json
# stdout: one JSON object only
```

Contract:

- **stdout**: single-line camelCase `DoctorSnapshot` (`schemaVersion=1`)
- **stderr**: tracing / error text only
- Fields include: `schemaVersion`, `generatedAt`, `status`, `version`, `platform`, `backend`, `paths`, `mdns`, `dependencies`, `recentErrors`, `logPath` (optional `logParseWarning`)

Illustrative shape (values abbreviated):

```json
{
  "schemaVersion": 1,
  "generatedAt": "2026-07-11T12:00:00Z",
  "status": "healthy",
  "version": { "app": "...", "backend": "..." },
  "platform": { "os": "macos", "arch": "aarch64" },
  "backend": {
    "state": "stopped",
    "controlPath": "<HOME>/.cc-partner/backend-control.json",
    "pid": null,
    "port": null,
    "health": { "status": "info", "code": "backend.stopped", "summary": "..." }
  },
  "paths": {
    "data": { "status": "ok", "code": "paths.data", "summary": "..." },
    "database": { "status": "ok", "code": "paths.database", "summary": "..." },
    "log": { "status": "ok", "code": "paths.log", "summary": "..." }
  },
  "mdns": { "status": "ok", "code": "mdns", "summary": "..." },
  "dependencies": {
    "git": { "status": "ok", "code": "deps.git", "summary": "..." },
    "tmux": { "status": "warning", "code": "deps.tmux", "summary": "..." },
    "wsl": { "status": "info", "code": "deps.wsl", "summary": "..." },
    "claudeCli": { "status": "ok", "code": "deps.claude_cli", "summary": "..." }
  },
  "recentErrors": [],
  "logPath": "<HOME>/.cc-partner/logs/backend.log"
}
```

Scripting tip: check process exit first; only then parse stdout as JSON for `doctor --json`.

## Privacy

- Diagnostics normalize home directories to `<HOME>` and redact secrets.
- No cloud log shipping, crash phone-home, or product telemetry from doctor/logs.
- Do not paste raw `backend-control.json` (contains `controlToken`) into tickets; use `status` / `doctor` instead.

## Related

- Product overview & quick CLI: [`README.md`](../../README.md)
- Quality gates & smoke matrix: [`testing.md`](testing.md)
- Backend engineering detail: [`src-tauri/CLAUDE.md`](../../src-tauri/CLAUDE.md)


## 配置与事务化运行时

- 权威配置文件：`<data_dir>/config.json`（可用 `CC_PARTNER_DATA_DIR` 隔离）。
- 写入顺序固定为 clone→mutate→validate→temp→fsync→re-read→atomic replace→dir fsync→memory swap。
- 故障注入：单元测试通过 `FaultInjectingConfigIo` 覆盖 create/write/flush/file-sync/rename/directory-sync；生产无对外故障注入开关。
- Cloud Sync：手动与 CLAUDE.md 推送 `Wait 300s`；scheduler `ReturnBusy` + `skippedBusy`。
- Updater 状态机在进程内，不持久化安装包字节。
- Health 非法磁盘配置：daemon 跳过提醒/清理本 tick，不 panic。
- **无** 本主题相关 SQLite 迁移/回滚脚本。
