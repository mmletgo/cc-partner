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
- Protocol capability tokens (e.g. `workbench.agent-runtime.v1`, `orchestrator.runtime-snapshot.v1`) are **version negotiation only** — they are not auth tokens and do not gate caller identity.

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

## Agent-first control CLI (`cc-partner`)

Standalone binary **`cc-partner`** (separate from lifecycle; **not** Tauri `externalBin`):

```text
cc-partner [--device local|id:<deviceId>] [--json] <resource> <action>
```

| Topic | Contract |
| --- | --- |
| Transport | Local: loopback control token + `POST /api/backend/control/agent/{query,mutate}`. Remote: explicit `id:<deviceId>` only; P2P business APIs; **never** send control token |
| Selectors | `id:`, `path:` (canonical), `branch:` exact; multi-hit → exit 4; no fuzzy / active / auto device |
| Bodies | `--input-json -` / stdin only (max 1 MiB; terminal send max 256 KiB); never argv / logs / error envelope |
| JSON | Success `{schemaVersion:1,ok:true,data}`; failure `{schemaVersion:1,ok:false,error:{code,message,retryable,requestId,outcomeUnknown}}` |
| Exit codes | 0 success · 1 internal · 2 usage · 3 not found · 4 conflict · 5 unavailable/timeout · 6 unsupported · 7 partial |
| Mutation | Query may refresh control file once; `NeverReplay` (session send / worktree create / browser verify) single hit; loss after dispatch → `outcomeUnknown=true` |
| Fixed LAN | Same credential-free business API boundary as P2P; peers are not “authenticated devices” |

```bash
cargo run --locked --bin cc-partner-cli -- --json project list
cargo run --locked --bin cc-partner-cli -- --device id:<deviceId> --json project list
printf '%s' '{"title":"t","goal":"g","acceptanceCriteria":"a"}' | \
  cargo run --locked --bin cc-partner-cli -- --json task create --project id:<pid> --input-json -
```

Smoke: `cd src-tauri && cargo test --locked --test agent_cli_smoke -- --nocapture --test-threads=1` (`L2-AGENT-CLI-SMOKE-001`).

## Runtime authority (sidecar owner)

The headless `serve` process is the sole **runtime owner** (`HeadlessOwner`) for
config, Cloud Sync engine, Workbench terminal/PTY, remote bridges, and Orchestrator
telemetry. The desktop GUI is a **GuiClient**: it may own window/tray/OS shortcuts and
forward Tauri events, but runtime mutations go through loopback control routes.

### Control file

`backend-control.json` (camelCase) includes at least:

- `pid`, `port`, `deviceId`, `deviceName`, `startedAt`, `controlToken`
- `controlSchemaVersion` (current **2**)
- `ownerInstanceId` (UUID generated once per sidecar process)

Legacy files missing schema/owner deserialize but are classified **stale / needs restart**
and must not be treated as an authoritative owner. CLI `status` JSON never prints the
control token. `controlSchemaVersion` / `ownerInstanceId` stay on the control file and
control status responses — they are **not** LAN health capabilities.

### Loopback control routes

All require **loopback peer + control token**. Metadata body ≤256 KiB / response ≤1 MiB
unless noted. Never transport-auto-retry mutations.

| Method | Path | Role |
| --- | --- | --- |
| POST | `/api/backend/control/stop` | graceful shutdown |
| POST | `/api/backend/control/status` | owner/generation + sanitized diagnostics |
| POST | `/api/backend/control/get-config` | authoritative config snapshot |
| POST | `/api/backend/control/update-config` | CAS allowlist patch (`expectedOwnerInstanceId` + `expectedGeneration`) |
| POST | `/api/backend/control/workbench` | Workbench metadata ops on owner (includes `agent_runtime.snapshot`) |
| POST | `/api/backend/control/workbench/data` | large file/browser payloads (body ≤32 MiB) |

### Agent session runtime (A1)

- Owner stores minimal metadata in `workbench_agent_sessions` (no prompt/response/terminal bytes/transcript path/credentials).
- `native_session_id` is owner-local only; never appears in Tauri/control/P2P/Mobile projection DTOs.
- Snapshot: control op `agent_runtime.snapshot` and P2P `POST /api/workbench/agent-runtime/snapshot` (capability `workbench.agent-runtime.v1`).
- Events: `workbench:agent-runtime` and NDJSON `type=agentRuntime` on `/api/workbench/events`; unknown event types must be ignored without reconnect.
- Downgrade: stop or end non-Claude active Agent sessions before rolling back a build that lacks agent-runtime projection; legacy dual-write of Claude fields remains one version.
| POST | `/api/backend/control/orchestrator/runtime-snapshot` | owner runtime snapshot |
| POST | `/api/backend/control/orchestrator/complete-agent-run` | owner complete agent run (verify/deliver) |
| POST | `/api/backend/control/orchestrator/dispatch-once` | owner one-shot scheduler tick |
| POST | `/api/backend/control/events/catch-up` | recovery / mixed-version fallback only（GUI normal path 不得周期性轮询） |
| POST | `/api/backend/control/events/stream` | **GUI normal path**：NDJSON catch-up + live；断线按 cursor 重连，Gap 先 replay 再 live |
| POST | `/api/backend/control/cloud-sync/trigger` | owner Cloud Sync full cycle |
| POST | `/api/backend/control/cloud-sync/test` | owner Cloud Sync connectivity |
| POST | `/api/backend/control/cloud-sync/claude-md-push` | owner CLAUDE.md Git push |
| POST | `/api/backend/control/backup/create` | verified export ZIP |
| POST | `/api/backend/control/backup/inspect` | streaming archive inspect (zero DB writes) |
| POST | `/api/backend/control/backup/restore` | exclusive-gate domain restore |
| POST | `/api/backend/control/backup/list-jobs` | list recovery_jobs |
| POST | `/api/backend/control/backup/list-backups` | list pre-restore backup ZIPs |
| POST | `/api/backend/control/backup/rollback` | rollback a recovery job |

`generation` increments only after a successful durable config replace. Wrong owner or
generation → conflict; GUI refreshes and user retries. Diagnostics copied from Settings
must omit tokens, Prompt/file/terminal content, and remote URL credentials.


## Ports & discovery

| Role | Protocol | Value |
| --- | --- | --- |
| Preferred P2P HTTP | TCP | **62116** (`DEFAULT_HTTP_PORT`) |
| Actual listen port | TCP | Preferred bind; on `AddrInUse` **increment by 1** until success → stored as actual |
| Config `http_port` `0` / invalid | — | Means “use preferred default”, **not** OS ephemeral `port=0` bind |
| Health field | `GET /api/health` → `http_port` | **Actual** listening port |
| Device discovery | UDP | **5353** (mDNS, service `_cc-partner._tcp.local.`) |

Desktop UI talks to Rust via Tauri `invoke` only — **no** local HTTP API port for the desktop frontend. Mobile SPA and peer P2P share the actual HTTP port.

### Mobile `/mobile` hot reload (dev only)

Production always serves the built SPA from Tauri embedded assets / `web/dist` on the backend HTTP port (preferred **62116**). During local development you can keep the **same QR / access URL** (`http://<LAN_IP>:<actual_http_port>/mobile`) while editing `web/src/mobile/**` with Vite HMR—no `npm run build` per change.

| Piece | Behavior |
| --- | --- |
| Entry | Phone & desktop browser open **backend** `/mobile` (not Vite `:5173` alone for API-correct mobile). |
| Proxy | Debug builds default **Auto**: HTTP fallback proxies `/mobile`, `/assets/*`, and Vite module paths (`/src/*`, `/@vite/*`, `/@react-refresh`, …) plus HMR WebSocket to `http://127.0.0.1:5173`. If Vite is down, shell/assets fall back to `web/dist`/embedded. |
| Release | Default **Off** (no loopback probe). |
| Env | `CC_PARTNER_MOBILE_DEV_PROXY=1\|0\|on\|off` force on/off. `CC_PARTNER_VITE_DEV_URL` overrides upstream (default `http://127.0.0.1:5173`). |
| Prerequisites | Vite running (`./start.sh` / Tauri `beforeDevCommand` / `cd web && npm run dev`) **and** backend listening (GUI sidecar after LAN disclosure, or `cc-partner-backend start`). |

Typical loop: start app/dev → open mobile access URL or QR → edit frontend → HMR/reload on device. Force dist-only: `CC_PARTNER_MOBILE_DEV_PROXY=0`.

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

## Backup export / restore (owner-only)

Verified data backup is an **owner maintenance** surface. Only the sidecar
`HeadlessOwner` may create, inspect, restore, list, or roll back archives.
Desktop GUI Settings → Sync tab proxies via loopback control routes (or Tauri
commands that rebind to the same control plane). Never expose these on the LAN
P2P surface without the control-file token + loopback peer check.

### Operator flow

1. **Export** — `POST /api/backend/control/backup/create` with a destination path
   chosen by the user (native save dialog on GUI). Produces a versioned ZIP with
   per-file SHA-256. Config is written as a **report-only** JSON (preview aid);
   it is **never** restored into `config.json`.
2. **Inspect before restore** — `POST .../backup/inspect` streams the archive with
   hard limits and **zero DB writes**:
   - archive size ≤ **2 GiB**
   - entries ≤ **100,000**
   - single entry uncompressed ≤ **64 MiB**
   - total uncompressed ≤ **4 GiB**
   - rejects zip-slip, absolute paths, symlinks, unknown `formatVersion`, and
     checksum mismatch
3. **Restore** — user selects domains + `merge` / `replace-domain` mode, then
   `POST .../backup/restore`. Owner takes the **exclusive** `DatabaseMaintenanceGate`
   lease, writes a pre-restore backup under the data directory, applies domains in
   a single SQLite transaction (+ index rebuild), and records a `recovery_jobs` row.
4. **Rollback** — `POST .../backup/rollback` with a job id re-applies that job's
   pre-restore backup under the same exclusive gate.

### Retention

- Pre-restore backups: keep **7 days** and at most **3** complete archives.
- Only delete older pre-restore backups **after** a new pre-restore archive has
  fully landed.

### Export exclusions (must never appear)

- Workbench project source trees
- Terminal transcripts / PTY buffers
- SSH private keys and other key material
- Tokens (including lifecycle `controlToken`)
- Credential URLs / secrets embedded in config report fields that would enable
  remote access

### Known verification scope (Task 6 smoke)

`backup_restore_smoke` and unit coverage exercise **inspect-level** reject/accept
paths, gate exclusivity, and recovery job shape. They are **not** a full process
kill/restart export→restore→export black-box on a live multi-process sidecar.
Treat full crash recovery matrices and secret-scanner sweeps as follow-up L3 /
manual evidence, not claimed by the smoke.

## Privacy

- Diagnostics normalize home directories to `<HOME>` and redact secrets.
- No cloud log shipping, crash phone-home, or product telemetry from doctor/logs.
- Do not paste raw `backend-control.json` (contains `controlToken`) into tickets; use `status` / `doctor` instead.
- Backup archives and inspect previews must not be shared if they contain user
  Prompt/Scratchpad text; control tokens never appear in export packages.

## Workbench terminal low-latency (P1 residual)

- Desktop normal path: control NDJSON **stream-first** (not 250ms catch-up poll).
- Input: max one in-flight write per session; failed/uncertain batches are **never** auto-replayed.
- **L3 GUI latency: NOT VERIFIED** as of `a25f8caa` — release GUI key-to-visible (p95≤50ms / p99≤100ms) and publish→listener (p95≤20ms) plus 1000 mixed-input ordering were not measured on a packaged app in this delivery. L0–L2 and P1 Superpowers dual review do **not** substitute for L3.

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

## Managed browser runtime (A5)

- Lock: `scripts/browser-runtime-lock.json` (Chrome for Testing headless-shell `150.0.7871.114`)
- Prepare: `node scripts/prepare-browser-runtime.mjs --platform current` (downloads into `.browser-runtime-cache/`, extracts to `src-tauri/resources/browser-runtime/`)
- Optional env override: `CC_PARTNER_BROWSER_RUNTIME` absolute path to chrome-headless-shell
- Capability: `workbench.browser-verification.v1` routes under `/api/workbench/browser-verification/*`
- Packaging status for all release platforms: see quality-matrix `L3-BROWSER-VERIFICATION-001` (NOT VERIFIED until real-device certification)
