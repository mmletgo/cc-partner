# P2P Protocol — Compatibility, Capability and Retry Policy

This document is the authoritative inventory of every HTTP route registered by
`src-tauri/src/net/http_server.rs`, classified by idempotency risk so that
client transport layers (Tauri invoke shims, `peer_client.rs`, the mobile
browser, and `cc-partner-backend`) know whether a request may be auto-replayed
after a network timeout.

The inventory is enforced by `scripts/check-p2p-route-inventory.mjs`, which
extracts literal `/api/...` paths from `http_server.rs` and from this document's
path column, then fails on any mismatch. **Adding or renaming a route requires
updating both the router and the table below in the same change.**

## Trust boundary (fixed, unauthenticated LAN)

cc-partner P2P/Mobile/Workbench/Orchestrator HTTP is **local/LAN only** and has
**one** fixed access behavior:

- Business routes do **not** authenticate caller identity. Any socket peer in the
  supported loopback/LAN ranges may read, write, and execute without credentials.
- Peer IP is taken only from TCP `ConnectInfo` (never forwarded headers). Supported
  ranges: IPv4 loopback / RFC1918 / IPv4 link-local, IPv6 loopback / ULA /
  link-local; IPv4-mapped IPv6 is normalized first.
- Listener may be wildcard `0.0.0.0` + actual TCP port (preferred **62116**,
  increment on conflict). Discovery uses mDNS UDP **5353**.
- Request guards: Host allow-list + actual port, Origin rules (native missing
  Origin allowed; ordinary APIs reject `Origin: null`; live Browser Preview may
  accept opaque null Origin only for that preview path), Content-Type simple-write
  rejection on ordinary APIs.
- Resource caps stay domain-specific (global **32 MiB**, transfer chunk **960 KiB**,
  text **5 MiB**, preview proxy **32 MiB**) — not a per-route authorization matrix.
- `POST /api/backend/control/stop` is local lifecycle only (loopback + control-file
  token). Business routes never require that token.

## Local control plane (not LAN business)

Routes under `/api/backend/control/*` are **local process control**, not LAN peer
APIs. They must stay separate from unauthenticated P2P/Mobile/Workbench/Orchestrator
business routes:

| Concern | LAN business `/api/*` (excl. control) | Local control `/api/backend/control/*` |
| --- | --- | --- |
| Peer scope | supported loopback/LAN ranges | **loopback socket only** (`ConnectInfo`) |
| Auth | none (no caller identity) | control-file **token** in JSON body |
| Capability ads | `server_protocol_info()` / health | **never** advertised as LAN capabilities |
| Version | `protocol_version` + capability tokens | `controlSchemaVersion` + `ownerInstanceId` live only in control file / status |
| Typical callers | peers, mobile browser, remote devices | GUI `BackendControlClient`, CLI stop |

Control inventory rows (status / get-config / update-config / workbench /
workbench-data / orchestrator runtime-snapshot / events catch-up / stream / stop)
share the table below so `check-p2p-route-inventory.mjs` can audit every literal
router path, but product and protocol docs must not describe `controlSchemaVersion`
as a LAN authorization capability or add it to `server_protocol_info()`.

Limits:
- metadata control JSON body ≤ **256 KiB**, ordinary metadata response ≤ **1 MiB**
- Workbench control data-plane body ≤ **32 MiB** (text/file/browser domain budgets stay 5/10/32 MiB on LAN routes)
- event NDJSON stream lines retain the separate **1 MiB** stream limit
- handlers must **never** log control tokens, Prompt/file/terminal content, or remote URL credentials

- Protocol capabilities (`attention.v1`, `errors.envelope.v1`, …) describe wire
  format / route existence only. There is **no** LAN permission capability token
  and no capability negotiation for LAN access. Old and new native peers remain
  credential-free.

Remaining risk wording (product/operator docs): 同一可达网络中的任何设备均可读取、写入和执行；系统不验证调用者身份. English equivalent: Any device on the same reachable network can read, write, and execute; the system does not verify caller identity.

Do **not** document configurable LAN modes, route authorization matrices, or claim
that LAN peers are authenticated / trusted / secure devices.

## Retry classes

Every route is assigned exactly one class:

| Class | Meaning | Transport retry policy |
| --- | --- | --- |
| `read-only` | No persistent mutation; safe to call any number of times. | Auto-retry allowed. |
| `naturally-idempotent` | Mutates state, but the implementation guarantees that replaying the same request converges to the same result (cited in the "Key / guard" column). | Auto-retry allowed. |
| `requires-idempotency-key` | Mutating and not yet replay-safe; the request MUST carry an idempotency key (e.g. `clientRequestId`) that the server deduplicates, OR clients MUST NOT auto-retry until a server-side dedupe is implemented. | Auto-retry only when the request carries the documented key; otherwise no transport-level retry. |
| `no-transport-retry` | Irreversible or externally observable side effect (git write, terminal bytes, process control, Orchestrator lifecycle). Retries can duplicate commits, double-feed terminal input, or race the state machine. | No automatic transport retry. Callers surface the failure and let the user decide. |

## Capability advertisement

`GET /api/health` returns `{protocol_version, capabilities}` (see
`src-tauri/src/net/protocol.rs`). New routes that depend on a wire-format
change must add a capability constant, advertise it in `server_protocol_info()`,
and gate the client call behind `PeerProtocolInfo::supports(...)`. The current
advertised capabilities are:

- `attention.v1` — Mobile Attention snapshot (`GET /api/mobile/attention`)
- `cc-history.paged-sync.v1` — bounded CC History paged sync (`POST /api/cc-history/sync/{manifest-page,items,push-batch}`); token and the three routes ship atomically
- `errors.envelope.v1` — standard error envelope wire format
- `orchestrator.review-diff.v1` — owning-device bounded Human Review / Rework diff (`POST /api/orchestrator/tasks/review-diff`); token and route ship atomically; mobile remote-aware wrapper at `/api/mobile/orchestrator/tasks/review-diff`. Display bounds: ≤200 files, total patch ≤2 MiB, single patch ≤256 KiB; binary metadata only; paths repo-relative (no `..`/absolute). `reviewDigest` = SHA-256 over base/head identity + sorted file metadata + full new-content hashes — **never** hashes truncated display patches. Owner Deliver must recollect and match non-empty `expectedReviewDigest` or conflict `review_diff_changed` (task stays Human Review)
- `orchestrator.runtime-snapshot.v1` — owning-device runtime snapshot route
- `orchestrator.workflow-document.v1` — owning-device WORKFLOW.md document get/validate/save (`POST /api/orchestrator/workflow-document/{get,validate,save}`); token and routes ship atomically; mobile remote-aware wrappers under `/api/mobile/orchestrator/workflow-document/*`. Save is CAS (`expectedHash` → conflict `workflow_document_changed`); **does not dispatch and cannot enable/change delivery** (Settings-only delivery)
- `sync.manifest.v2` — bounded Prompt / SSH target / Scratchpad content sync (`POST /api/sync/prompts/{manifest-page,items,push-batch,ack-delete-epoch}`, `/api/ssh-target/sync/{...}`, `/api/scratchpad/sync/{...}`); token ships with transactional bulk upsert + request ledger + apply_merge_batch + dedicated watermark ack
- `transfer.complete.v1` — explicit transfer finalize handshake (`POST /api/transfer/complete/:id`)
- `transfer.resume.v1` — sender-side resume recovery (stable `protocolTransferId` + source fingerprint + peer checkpoint). Token ships with `resume_transfer` / operation ledger claim; **no new LAN route** — wire still uses init/chunk/complete/status keyed by the stable protocol id
- `workbench.mutation-outcome.v1` — Workbench Git mutation envelope (`succeeded|unknown`) + durable operation ledger query (`POST /api/workbench/worktrees/mutation-operation`); token ships with commit/push/merge/remove envelope responses

### Semantics of `errors.envelope.v1` (important)

`errors.envelope.v1` describes the **error response wire format only**
(`P2pErrorEnvelope`: `error`/`code`/`request_id`/`retryable`/`details`), **not**
route access or route existence. Concretely:

- A v0 peer that does **not** advertise this token still has all of its existing
  `/api/...` routes callable. Only its error responses may arrive in the legacy
  `{error: "..."}` shape, which the client must tolerate via
  `parse_peer_response` (it auto-detects legacy vs v1 envelope).
- The token does **not** mean "this peer implements a particular new route".
  Confirming route existence is a separate concern (health/version probe or
  handling 404). Do not use `supports("errors.envelope.v1")` as a proxy for
  "new routes are available".
- Route-specific capabilities (`attention.v1`, `cc-history.paged-sync.v1`,
  `orchestrator.review-diff.v1`, `orchestrator.runtime-snapshot.v1`,
  `orchestrator.workflow-document.v1`, `sync.manifest.v2`, `transfer.complete.v1`,
  `transfer.resume.v1`, …) ship as **independent** tokens alongside their own
  contracts and must not reuse `errors.envelope.v1` to mean "new routes supported".

The existing capability gate (`peer_client::require_capability`) is therefore a
**format** gate when used with `errors.envelope.v1`, and a **route** gate when
used with route-specific tokens: it lets a caller avoid sending a request the
peer cannot serve. It does not restrict which routes a peer may call beyond the
capability contract.

## Route inventory

The "Path" column mirrors the literal string passed to axum `.route(...)`.
Dynamic segments (`:id`, `:previewId`, `*path`) are documented identically to
the router so the inventory check matches exactly.

| Method | Path | Owner | Side effect | Retry class | Key / guard |
| --- | --- | --- | --- | --- | --- |
| GET | `/api/health` | `routes/health.rs` | none | read-only | — |
| POST | `/api/backend/control/stop` | `http_server.rs` | signals local `serve` shutdown via control-token gate | no-transport-retry | `controlToken` must match control file; retry after timeout can hit a recycled port |
| POST | `/api/backend/control/status` | `backend/control_api.rs` | none; returns owner/generation runtime status | read-only | loopback + control-file token; body ≤256 KiB; response ≤1 MiB; never logs token |
| POST | `/api/backend/control/get-config` | `backend/control_api.rs` | none; returns authoritative config snapshot | read-only | loopback + control-file token; body ≤256 KiB; response ≤1 MiB |
| POST | `/api/backend/control/workbench-launch-summary` | `backend/control_api.rs` | none; bounded launch summary with independent section outcomes | read-only | loopback + control-file token; body `{controlToken}` ≤256 KiB; response ≤1 MiB; max 5 per section; never logs token |
| POST | `/api/backend/control/update-config` | `backend/control_api.rs` | CAS patch of runtime config via owner/generation | no-transport-retry | loopback + control-file token; `expectedOwnerInstanceId` + `expectedGeneration` + allowlisted `RuntimeConfigPatch` (`deny_unknown_fields`); generation conflict → 409; body ≤256 KiB; response ≤1 MiB |
| POST | `/api/backend/control/workbench` | `backend/control_workbench.rs` | dispatches Workbench metadata ops (projects/worktrees/git/files/sessions/claude) on sidecar owner | no-transport-retry | loopback + control-file token; body `{controlToken,op,payload}` camelCase ≤256 KiB; response `{ownerInstanceId,result}` ≤1 MiB; requires HeadlessOwner; many ops mutate so clients must not transport-retry |
| POST | `/api/backend/control/workbench/data` | `backend/control_workbench.rs` | same dispatcher for large payload/response ops (`files.save_text`/`files.open`/`files.preview_*`/`browser.*`) | no-transport-retry | loopback + control-file token; body ≤32 MiB; response not capped at 1 MiB; never transport-retry |
| POST | `/api/backend/control/orchestrator/runtime-snapshot` | `backend/control_api.rs` | none; returns sidecar remote-aware Orchestrator runtime snapshot | read-only | loopback + control-file token; body `{controlToken,projectId}` ≤256 KiB; response ≤1 MiB; GUI must not fill owner fields from local empty telemetry |
| POST | `/api/backend/control/orchestrator/deliver-reviewed` | `backend/control_api.rs` | owner full delivery pipeline (digest enforce + Settings gate + commit/push/merge) | no-transport-retry | loopback + control-file token; requires HeadlessOwner; body `{controlToken,projectId,taskId,expectedReviewDigest?}` camelCase ≤256 KiB; response `OrchestratorTaskViewDto` ≤1 MiB; GuiClient must proxy — sole Git delivery authority; never transport-retry |
| POST | `/api/backend/control/orchestrator/review-diff` | `backend/control_api.rs` | none; owner remote-aware review diff from task worktree | no-transport-retry | loopback + control-file token; requires HeadlessOwner; body `{controlToken,projectId,taskId}` camelCase ≤256 KiB; response `OrchestratorReviewDiff` (bounded patch may exceed 1 MiB metadata cap); GUI must not read local empty worktree for digest authority |
| POST | `/api/backend/control/orchestrator/workflow-document/get` | `backend/control_api.rs` | none; owner remote-aware WORKFLOW.md status | no-transport-retry | loopback + control-file token; requires HeadlessOwner; body `{controlToken,projectId}` camelCase ≤256 KiB; response `WorkflowDocument` ≤1 MiB |
| POST | `/api/backend/control/orchestrator/workflow-document/validate` | `backend/control_api.rs` | none; owner authoritative WORKFLOW parser | no-transport-retry | loopback + control-file token; requires HeadlessOwner; body `{controlToken,projectId,content}` camelCase ≤256 KiB; response `WorkflowDocument` ≤1 MiB; never transport-retry |
| POST | `/api/backend/control/orchestrator/workflow-document/save` | `backend/control_api.rs` | owner CAS exclusive create / hash-gated write of project-root WORKFLOW.md | no-transport-retry | loopback + control-file token; requires HeadlessOwner; body `{controlToken,projectId,expectedHash,content}` camelCase ≤256 KiB; response `WorkflowDocument` ≤1 MiB; never dispatch/delivery; never transport-retry |
| POST | `/api/backend/control/operational-notifications/snapshot` | `backend/control_api.rs` | none; returns privacy-safe operational notification baseline snapshot | read-only | loopback + control-file token; body `{controlToken}` ≤256 KiB; response `{asOfCursor,items,truncated}` ≤1 MiB; items ≤1000 opaque states `{kind,opaqueSourceId,stateVersion,occurredAt}` only (no title/goal/project/diff/evidence); live event `operational:notification` same privacy shape; **not** a LAN capability and not advertised in health `capabilities` |
| POST | `/api/backend/control/events/catch-up` | `backend/control_api.rs` | none; returns afterSequence replay / Gap from owner event bus | read-only | loopback + control-file token; body `{controlToken,afterOwnerInstanceId?,afterSequence?}` ≤256 KiB; response ≤1 MiB; cursor is `(ownerInstanceId,sequence)` |
| POST | `/api/backend/control/events/stream` | `backend/control_api.rs` | none; NDJSON catch-up then live owner events | read-only | loopback + control-file token; cancellable stream; same afterSequence semantics as catch-up; never logs token |
| POST | `/api/backend/control/cloud-sync/trigger` | `backend/control_api.rs` | runs owner CloudSyncRuntime full sync (Wait gate) | no-transport-retry | loopback + control-file token; requires HeadlessOwner; shares single Git workdir critical section with scheduler; body `{controlToken}` ≤256 KiB; never transport-retry |
| POST | `/api/backend/control/cloud-sync/test` | `backend/control_api.rs` | may fetch workdir under owner gate for connectivity | no-transport-retry | loopback + control-file token; requires HeadlessOwner; body `{controlToken}` ≤256 KiB |
| POST | `/api/backend/control/cloud-sync/claude-md-push` | `backend/control_api.rs` | owner-side CLAUDE.md Git workdir export/commit/push | no-transport-retry | loopback + control-file token; requires HeadlessOwner; body `{controlToken,content,updatedAt,deviceId,vectorClock}`; shares CloudSyncRuntime with trigger/scheduler |
| POST | `/api/backend/control/backup/create` | `backend/control_api.rs` | writes verified export ZIP at destPath from owner DB | no-transport-retry | loopback + control-file token; requires HeadlessOwner; body `{controlToken,destPath}` ≤256 KiB; response `{path,formatVersion}`; export excludes project source, terminal transcripts, private keys, tokens, credential URLs, control tokens; config is report-only; never transport-retry; never logs token |
| POST | `/api/backend/control/backup/inspect` | `backend/control_api.rs` | none; streaming checksum inspect + domain counts | no-transport-retry | loopback + control-file token; requires HeadlessOwner; body `{controlToken,archivePath}` ≤256 KiB; response `InspectPreview` ≤1 MiB; **zero DB writes**; streaming limits archive ≤2 GiB / entries ≤100k / entry ≤64 MiB / total ≤4 GiB; rejects zip-slip, symlink, absolute path, unknown formatVersion, hash mismatch |
| POST | `/api/backend/control/backup/restore` | `backend/control_api.rs` | exclusive maintenance_gate restore (pre-restore backup + domain apply) | no-transport-retry | loopback + control-file token; requires HeadlessOwner; body `{controlToken,archivePath,mode,domains}` ≤256 KiB; response `RestoreResult`; exclusive DB lease; pre-restore backup retained 7d / max 3 (delete old only after new complete); config report never restored; never transport-retry |
| POST | `/api/backend/control/backup/list-jobs` | `backend/control_api.rs` | none; lists recovery_jobs | read-only | loopback + control-file token; requires HeadlessOwner; body `{controlToken,limit?}` ≤256 KiB; default limit 50; response ≤1 MiB |
| POST | `/api/backend/control/backup/list-backups` | `backend/control_api.rs` | none; lists pre-restore ZIP paths under data_dir | read-only | loopback + control-file token; requires HeadlessOwner; body `{controlToken}` ≤256 KiB; response `[{path,createdAt?}]` ≤1 MiB |
| POST | `/api/backend/control/backup/rollback` | `backend/control_api.rs` | replace-domain restore from pre-restore backup of a job | no-transport-retry | loopback + control-file token; requires HeadlessOwner; body `{controlToken,jobId}` ≤256 KiB; response `RestoreResult`; exclusive gate; never transport-retry |
| POST | `/api/backend/control/transfer/prepare-open` | `backend/control_api.rs` | none; validates Receive+completed+path exists and returns local open target | no-transport-retry | loopback + control-file token; requires HeadlessOwner; body `{controlToken,taskId,action}` camelCase `action=open|reveal` ≤256 KiB; response `LocalTransferOpenTarget{taskId,action,path}` ≤1 MiB; **never** exposed on LAN/P2P/mobile; path is same-device only; stable errors `transfer_not_completed` / `transfer_path_missing` / `transfer_path_invalid` / `unsupported` |
| POST | `/api/backend/control/transfer/send` | `backend/control_api.rs` | owner starts send (registry + spawn) | no-transport-retry | loopback + control-file token; requires HeadlessOwner; body `{controlToken,deviceId,filePath}` ≤256 KiB; response `{accepted,deviceId,filePath,id}`; GuiClient only — sole runtime owner pattern |
| POST | `/api/backend/control/transfer/retry` | `backend/control_api.rs` | owner idempotent retry claim+spawn | no-transport-retry | loopback + control-file token; requires HeadlessOwner; body `{controlToken,taskId,clientOperationId}` ≤256 KiB; response `TransferTaskDto`; same op id + stable retry payload hash → Replay; different payload → operationIdConflict |
| POST | `/api/backend/control/transfer/resume` | `backend/control_api.rs` | owner idempotent resume claim+spawn | no-transport-retry | loopback + control-file token; requires HeadlessOwner; body `{controlToken,taskId,clientOperationId}` ≤256 KiB; response `TransferTaskDto`; requires peer `transfer.resume.v1` |
| POST | `/api/backend/control/transfer/get-operation` | `backend/control_api.rs` | none; owner ledger + lost-ACK reconcile | no-transport-retry | loopback + control-file token; requires HeadlessOwner; body `{controlToken,clientOperationId}` ≤256 KiB; response `TransferOperationStatus` |
| POST | `/api/backend/control/transfer/cancel` | `backend/control_api.rs` | owner cancels registry token | no-transport-retry | loopback + control-file token; requires HeadlessOwner; body `{controlToken,taskId}` ≤256 KiB; response `{ok,id}` |
| GET | `/api/mobile/access-info` | `routes/mobile.rs` | none | read-only | — |
| GET | `/api/mobile/attention` | `routes/attention.rs` | none; aggregates local Attention snapshot | read-only | capability-gated by `attention.v1`; reuses `list_attention_items_for_state`; may refresh each remote owning device once via orchestrator source, never recursively asks another device to aggregate attention |
| POST | `/api/sync/pull` | `routes/sync.rs` | none; returns rows the caller is missing | read-only | vector-clock comparison only reads local DB |
| POST | `/api/sync/push` | `routes/sync.rs` | upserts prompt rows after vector-clock merge | naturally-idempotent | `sync_push_impl` re-merges each row; `bulk_upsert` with merged clock converges on replay |
| POST | `/api/sync/prompts/manifest-page` | `routes/sync.rs` | none; keyset page of `SyncSummary` (id/vector_clock/content_hash/size/updated_at/deleted/delete_epoch) | read-only | capability-gated by `sync.manifest.v2` (advertised with transactional apply + ledger); body `{cursor?,limit?}` snake_case; default/max page **500 items / 1 MiB** estimate; response `{items,next_cursor}`; **opaque** base64url cursor; illegal cursor → 400 `prompts.invalid_cursor`; client must stream until `next_cursor=None` before classifying remote-only data or advancing delete ack |
| POST | `/api/sync/prompts/items` | `routes/sync.rs` | none; returns existing full `PromptRow` for requested ids | read-only | capability-gated by `sync.manifest.v2`; body `{ids}` max **100** / 4 MiB; response ordered `{items,missing_ids}`; estimate ≤4 MiB else 413 `prompts.batch_too_large` (`retryable=false`); single content >1 MiB → 422 `prompts.item_too_large` |
| POST | `/api/sync/prompts/push-batch` | `routes/sync.rs` | single-transaction `apply_prompt_merge_batch` (ledger + winners + conflict versions + delete_epoch + optional watermark ack) | naturally-idempotent | capability-gated by `sync.manifest.v2`; body `{items:PromptRow[],client_request_id,claimed_device_id?,acked_delete_epoch?}` max **100 / 4 MiB**; empty `client_request_id` → 400; `claimed_device_id` is a convergence **label not auth**; ledger `UNIQUE(claimed_device_id,domain,client_request_id)` + payload hash — same key/hash → recorded `{accepted}`; same key/different hash → **409 conflict**; incomplete manifest must not send ack; **empty content batches must not be used solely for ack** — use `ack-delete-epoch` instead; last content batch may still carry `acked_delete_epoch` |
| POST | `/api/sync/prompts/ack-delete-epoch` | `routes/sync.rs` | advances peer/domain `acked_delete_epoch` only; no content upsert/ledger | naturally-idempotent | capability-gated by `sync.manifest.v2`; body `{claimed_device_id,acked_delete_epoch}` snake_case; empty/blank `claimed_device_id` → 400 Validation; Key/guard: `SyncWatermarkRepo::advance_ack` MAX-only (replay-safe); response `{ok:true}` |
| POST | `/api/sync/claude_md/pull` | `routes/claude_md_sync.rs` | none; returns the singleton row | read-only | — |
| POST | `/api/sync/claude_md/push` | `routes/claude_md_sync.rs` | overwrites singleton + `~/.claude/CLAUDE.md` | naturally-idempotent | sender only pushes its own already-merged version; re-applying the same row + `write_file_if_changed` is a no-op |
| POST | `/api/transfer/init` | `routes/transfer.rs` | creates `.{transfer_id}.tmp` + receive registry entry | requires-idempotency-key | Wire `transfer_id` **is** the durable `protocolTransferId`. Resume reuses the same id to hit receiver checkpoint/tmp; full retry may mint a new id. Server does not enforce that a transport replay reuses the same id — minting a fresh id leaks a tmp + registry entry. Clients MUST reuse the original protocol id on resume/transport replay or MUST NOT auto-retry. tmp larger than declared size is rejected and deleted |
| POST | `/api/transfer/chunk/:id` | `routes/transfer.rs` | writes bytes at offset, finalizes when complete | no-transport-retry | the final chunk triggers `finalize_transfer` which atomically places the tmp file into its final path via hard_link(tmp,final) no-replace commit (receive_dir lock; fallback rename_no_replace) and removes the registry entry; a transport-layer replay of the same final-chunk request previously hit a missing registry entry and returned `success:false`. The receiver now guards finalize with a per-`transfer_id` singleflight lock + a short-lived terminal tombstone, so a duplicate final chunk returns the first finalize's result — but middle chunks still mutate the tmp file at arbitrary offsets and there is no per-offset dedupe, so transport-layer retries must be disabled; callers surface the failure and let the user re-initiate |
| POST | `/api/transfer/complete/:id` | `routes/transfer.rs` | SHA256 verify + atomic place when bytes are complete (incl. size=0 / full-tmp resume) | naturally-idempotent | capability-gated by `transfer.complete.v1`; sender only calls when peer advertises the token; legacy peers without it use last-chunk finalize for non-empty transfers and fail size=0/full-tmp as unsupported; tombstone makes replay return the first terminal outcome; client does bounded retries on network/5xx then status fallback. Hash mismatch fails finalize and does **not** place the file. Lost ACK: sender reconciles via `GET /api/transfer/status/:id` and must **not** invoke a second destructive finalize |
| GET | `/api/transfer/status/:id` | `routes/transfer.rs` | none | read-only | authoritative receiver outcome for lost-ACK / uncertain reconcile; keyed only by `protocolTransferId` (not sender `clientOperationId`) |
| POST | `/api/cc-history/sync/pull` | `routes/cc_history.rs` | none | read-only | legacy full-summary pull retained for mixed-version peers that lack `cc-history.paged-sync.v1` |
| POST | `/api/cc-history/sync/push` | `routes/cc_history.rs` | upserts CC history rows after merge | naturally-idempotent | legacy full-body push; per-row `merge_cc_history` + `bulk_upsert`; replay converges |
| POST | `/api/cc-history/sync/manifest-page` | `routes/cc_history.rs` | none; keyset page of `{id,vector_clock}` summaries | read-only | capability-gated by `cc-history.paged-sync.v1`; body `{cursor?,limit?}` snake_case; default limit 256, max 512; response `{summaries,next_cursor,done}`; **cursor is opaque** (base64url JSON `{v:1,last_id}`) — clients must not parse; illegal cursor → 400 `cc_history.invalid_cursor`; route body limit 8 MiB |
| POST | `/api/cc-history/sync/items` | `routes/cc_history.rs` | none; returns existing full rows for requested ids | read-only | capability-gated by `cc-history.paged-sync.v1`; body `{ids:string[]}` max 128, each id ≤256 UTF-8 bytes, no blanks/dupes; response ordered `{items,missing_ids}`; estimated response ≤8 MiB else 413 `cc_history.batch_too_large` (`retryable=false`); single content >1 MiB → 422 `cc_history.item_too_large`; clients may bisect a 413 batch down to one id |
| POST | `/api/cc-history/sync/push-batch` | `routes/cc_history.rs` | merge + **single-transaction** upsert of a batch | naturally-idempotent | capability-gated by `cc-history.paged-sync.v1`; body `{items:ClaudeHistoryRow[]}` max 128 / 8 MiB estimate / 1 MiB content; `merge_cc_history` then `upsert_merged_batch` (all-or-nothing — **no partial accepted**); returns `{accepted}`; same limits/error codes as items; replay converges via vector-clock merge |
| POST | `/api/ssh-target/sync/pull` | `routes/ssh_target_sync.rs` | none | read-only | — |
| POST | `/api/ssh-target/sync/push` | `routes/ssh_target_sync.rs` | upserts SSH target rows after merge | naturally-idempotent | per-row `merge_ssh_target` + `bulk_upsert`; replay converges |
| POST | `/api/ssh-target/sync/manifest-page` | `routes/ssh_target_sync.rs` | none; keyset page of SSH `SyncSummary` (id=host) | read-only | capability-gated by `sync.manifest.v2`; same budgets as prompts v2 (500/1 MiB, opaque cursor); illegal cursor → 400 `ssh_target.invalid_cursor` |
| POST | `/api/ssh-target/sync/items` | `routes/ssh_target_sync.rs` | none; returns existing full `SshTargetRow` for requested hosts | read-only | capability-gated by `sync.manifest.v2`; body `{ids}` max 100 / 4 MiB; 413 `ssh_target.batch_too_large` |
| POST | `/api/ssh-target/sync/push-batch` | `routes/ssh_target_sync.rs` | single-transaction `apply_ssh_merge_batch` (ledger + upsert + conflicts + epochs) | naturally-idempotent | capability-gated by `sync.manifest.v2`; body `{items,client_request_id,claimed_device_id?,acked_delete_epoch?}` max 100 / 4 MiB; ledger UNIQUE key + hash rules identical to prompts; 409 on same key/different hash; empty content batches must not be used solely for ack — use `ack-delete-epoch` instead; last content batch may still carry `acked_delete_epoch` |
| POST | `/api/ssh-target/sync/ack-delete-epoch` | `routes/ssh_target_sync.rs` | advances peer/domain `acked_delete_epoch` only; no content upsert/ledger | naturally-idempotent | capability-gated by `sync.manifest.v2`; body `{claimed_device_id,acked_delete_epoch}` snake_case; empty/blank `claimed_device_id` → 400 Validation; Key/guard: `SyncWatermarkRepo::advance_ack` MAX-only (replay-safe); response `{ok:true}` |
| POST | `/api/scratchpad/sync/pull` | `routes/scratchpad_sync.rs` | none | read-only | — |
| POST | `/api/scratchpad/sync/push` | `routes/scratchpad_sync.rs` | upserts scratchpad pages after merge | naturally-idempotent | per-row `merge_scratchpad` + `bulk_upsert`; replay converges |
| POST | `/api/scratchpad/sync/manifest-page` | `routes/scratchpad_sync.rs` | none; keyset page of scratchpad `SyncSummary` | read-only | capability-gated by `sync.manifest.v2`; same budgets as prompts v2 (500/1 MiB, opaque cursor); illegal cursor → 400 `scratchpad.invalid_cursor` |
| POST | `/api/scratchpad/sync/items` | `routes/scratchpad_sync.rs` | none; returns existing full `ScratchpadRow` for requested ids | read-only | capability-gated by `sync.manifest.v2`; body `{ids}` max 100 / 4 MiB; 413 `scratchpad.batch_too_large` |
| POST | `/api/scratchpad/sync/push-batch` | `routes/scratchpad_sync.rs` | single-transaction `apply_scratchpad_merge_batch` (ledger + upsert + conflicts + epochs) | naturally-idempotent | capability-gated by `sync.manifest.v2`; body `{items,client_request_id,claimed_device_id?,acked_delete_epoch?}` max 100 / 4 MiB; ledger UNIQUE key + hash rules identical to prompts; 409 on same key/different hash; empty content batches must not be used solely for ack — use `ack-delete-epoch` instead; last content batch may still carry `acked_delete_epoch` |
| POST | `/api/scratchpad/sync/ack-delete-epoch` | `routes/scratchpad_sync.rs` | advances peer/domain `acked_delete_epoch` only; no content upsert/ledger | naturally-idempotent | capability-gated by `sync.manifest.v2`; body `{claimed_device_id,acked_delete_epoch}` snake_case; empty/blank `claimed_device_id` → 400 Validation; Key/guard: `SyncWatermarkRepo::advance_ack` MAX-only (replay-safe); response `{ok:true}` |
| GET | `/api/claude-code/assets/inventory` | `routes/claude_code_assets.rs` | none | read-only | — |
| POST | `/api/claude-code/assets/bundle` | `routes/claude_code_assets.rs` | builds an in-memory zip; no persistent mutation | read-only | — |
| GET | `/api/workbench/fs/roots` | `routes/workbench.rs` | none | read-only | — |
| POST | `/api/workbench/fs/list` | `routes/workbench.rs` | none | read-only | — |
| POST | `/api/workbench/fs/info` | `routes/workbench.rs` | none | read-only | — |
| GET | `/api/workbench/projects/list` | `routes/workbench.rs` | none | read-only | — |
| POST | `/api/workbench/projects/open` | `routes/workbench.rs` | upserts a `local` project row keyed by canonical path | naturally-idempotent | `add_workbench_project` reuses the same project id for the same path and only refreshes timestamps |
| POST | `/api/workbench/worktrees/list` | `routes/workbench.rs` | none (reconciles existing worktrees into SQLite) | read-only | — |
| POST | `/api/workbench/worktrees/create` | `routes/workbench.rs` | `git worktree add` + new SQLite row | requires-idempotency-key | no dedupe key yet; clients MUST NOT auto-retry until a worktree-create idempotency key lands |
| POST | `/api/workbench/worktrees/get` | `routes/workbench.rs` | none | read-only | — |
| POST | `/api/workbench/worktrees/commit` | `routes/workbench.rs` | `git add -A` + `git commit` | requires-idempotency-key | when body carries `clientOperationId`: UNIQUE ledger (`workbench_mutation_operations`) same id/same payload replays; different payload → conflict; response is `workbench.mutation-outcome.v1` envelope. when id omitted: legacy raw worktree DTO for old peers (no envelope) |
| POST | `/api/workbench/worktrees/push` | `routes/workbench.rs` | `git push` of the task branch | requires-idempotency-key | same ledger/envelope contract as commit; remote-ref postcondition for reconcile |
| POST | `/api/workbench/worktrees/merge` | `routes/workbench.rs` | `git merge --no-ff`, conflict resolution, branch delete | requires-idempotency-key | same ledger/envelope contract; source/main HEAD intent |
| POST | `/api/workbench/worktrees/remove` | `routes/workbench.rs` | `git worktree remove` + row delete | requires-idempotency-key | same ledger/envelope contract; exact worktree identity intent |
| POST | `/api/workbench/worktrees/mutation-operation` | `routes/workbench.rs` | none; reads durable ledger by `clientOperationId` | read-only | capability `workbench.mutation-outcome.v1` |
| POST | `/api/workbench/git/commits` | `routes/workbench.rs` | none; `git log` read | read-only | — |
| POST | `/api/workbench/files/list-dir` | `routes/workbench.rs` | none | read-only | — |
| POST | `/api/workbench/files/info` | `routes/workbench.rs` | none | read-only | — |
| POST | `/api/workbench/files/open` | `routes/workbench.rs` | none; reads file content + metadata | read-only | — |
| POST | `/api/workbench/files/save-text` | `routes/workbench.rs` | writes worktree-relative text file | naturally-idempotent | `baseHash` optimistic-lock guard in `local_save_workbench_text_file`; a stale/replayed hash is rejected |
| POST | `/api/workbench/files/preview-sqlite` | `routes/workbench.rs` | none; read-only table enumeration | read-only | — |
| POST | `/api/workbench/files/preview-html-asset` | `routes/workbench.rs` | none | read-only | — |
| POST | `/api/workbench/files/create-file` | `routes/workbench.rs` | creates a worktree-relative file | requires-idempotency-key | no dedupe key yet; replay after partial failure can collide with an already-created file |
| POST | `/api/workbench/files/create-dir` | `routes/workbench.rs` | creates a worktree-relative directory | requires-idempotency-key | no dedupe key yet; clients MUST NOT auto-retry until a create-idempotency key lands |
| POST | `/api/workbench/files/rename` | `routes/workbench.rs` | irreversible `fs::rename` inside worktree root | no-transport-retry | source path disappears; replay after timeout can fail or rename the wrong target |
| POST | `/api/workbench/files/delete` | `routes/workbench.rs` | irreversible delete inside worktree root | no-transport-retry | destructive |
| GET | `/api/workbench/events` | `routes/workbench.rs` | none; NDJSON event stream with typed heartbeat every 15s (`{"type":"heartbeat","sentAt":"<RFC3339>"}`) | read-only | client uses lifecycle + per-connection abort; 35s watchdog aborts only the child connection and reconnects while lifecycle remains active; business frames and heartbeats both reset the watchdog |
| POST | `/api/workbench/sessions/list` | `routes/workbench.rs` | `restore_persisted_sessions` may recreate missing tmux windows for persisted session rows | no-transport-retry | `local_list_workbench_sessions` → `restore_persisted_sessions` may spawn a PTY/tmux window for each persisted row. The registry now guards restore with an atomic `try_claim_restore` placeholder (Finding 5) so concurrent list requests no longer double-restore the same row, but a transport-layer replay still triggers real subprocess spawns and tmux window creation with no per-request dedupe key; callers surface the failure and let the user re-list explicitly |
| POST | `/api/workbench/sessions/create` | `routes/workbench.rs` | new tmux window / PTY + SQLite window row | requires-idempotency-key | no dedupe key yet; clients MUST NOT auto-retry until a session-create idempotency key lands |
| POST | `/api/workbench/sessions/replay` | `routes/workbench.rs` | none; reads ring buffer | read-only | — |
| POST | `/api/workbench/sessions/write` | `routes/workbench.rs` | writes bytes to PTY/tmux pane stdin | no-transport-retry | appended input has no offset guard; replay duplicates keystrokes/commands |
| POST | `/api/workbench/sessions/resize` | `routes/workbench.rs` | sets tmux/window cols+rows, persists size | naturally-idempotent | same `(cols, rows)` is a no-op; `tmux resize` and the size row converge on replay |
| POST | `/api/workbench/sessions/focus` | `routes/workbench.rs` | `tmux select-window` | naturally-idempotent | selecting the same window target is a no-op |
| POST | `/api/workbench/sessions/focused` | `routes/workbench.rs` | none; `tmux display-message` query | read-only | — |
| POST | `/api/workbench/sessions/split-pane` | `routes/workbench.rs` | `tmux split-window` creates a new pane | requires-idempotency-key | no dedupe key yet; replay creates a second pane |
| POST | `/api/workbench/sessions/switch-pane` | `routes/workbench.rs` | `tmux select-pane -t .+` cycles to the next pane | no-transport-retry | `switch_to_next_pane` runs a relative `select-pane` cycle; a transport replay after a timeout lands on a *different* pane than the caller intended, so the client must not auto-replay |
| POST | `/api/workbench/sessions/zoom-pane` | `routes/workbench.rs` | `tmux resize-pane -Z` guarded by current zoom flag | naturally-idempotent | `ensure_active_pane_zoomed` checks `#{window_zoomed_flag}` before toggling |
| POST | `/api/workbench/sessions/close-pane` | `routes/workbench.rs` | `tmux kill-pane`/`kill-window` + row delete | no-transport-retry | destructive |
| POST | `/api/workbench/sessions/close` | `routes/workbench.rs` | `tmux kill-window`/`kill-session` + row delete | no-transport-retry | destructive |
| POST | `/api/workbench/sessions/rename` | `routes/workbench.rs` | persists name + `tmux rename-window` | naturally-idempotent | same name replayed is a no-op on both SQLite and tmux |
| POST | `/api/workbench/claude-sessions/search` | `routes/workbench.rs` | none; scans jsonl index | read-only | — |
| POST | `/api/workbench/claude-sessions/preview` | `routes/workbench.rs` | none | read-only | — |
| POST | `/api/workbench/claude-sessions/resume` | `routes/workbench.rs` | creates a new window + writes `claude --resume` command | requires-idempotency-key | creates a session then writes to it; no dedupe key yet, clients MUST NOT auto-retry |
| POST | `/api/workbench/prompt-optimizer/stream-to-session` | `routes/workbench.rs` | spawns Claude CLI and streams text into a terminal | no-transport-retry | paid, nondeterministic LLM call whose output is appended to the pane; replay duplicates tokens |
| POST | `/api/workbench/browser/discover` | `routes/workbench.rs` | none; scans loopback dev servers | read-only | — |
| POST | `/api/workbench/browser/preview` | `routes/workbench.rs` | registers a previewId (local relay or remote relay) | requires-idempotency-key | each call mints a new UUID previewId; replay registers a second stale entry |
| ANY | `/api/workbench/browser/proxy/:previewId/*path` | `routes/workbench.rs` | proxied HTTP/WS pass-through; forwards arbitrary methods (GET/POST/PUT/DELETE) to the upstream dev server | no-transport-retry | the proxy is method-agnostic (`any(...)`); a retry can replay a non-idempotent upstream POST/PUT/DELETE, so the transport layer must not auto-replay proxied requests |
| GET | `/api/mobile/workbench/projects/list` | `routes/workbench.rs` | none | read-only | — |
| POST | `/api/mobile/workbench/projects/open` | `routes/workbench.rs` | upserts a `local` project row keyed by canonical path | naturally-idempotent | reuses same project id for same path |
| POST | `/api/mobile/workbench/worktrees/list` | `routes/workbench.rs` | none | read-only | — |
| POST | `/api/mobile/workbench/worktrees/create` | `routes/workbench.rs` | `git worktree add` + new SQLite row | requires-idempotency-key | no dedupe key yet; clients MUST NOT auto-retry |
| POST | `/api/mobile/workbench/worktrees/commit` | `routes/workbench.rs` | `git add -A` + `git commit` | requires-idempotency-key | mobile shares the same `clientOperationId` ledger/envelope as P2P commit |
| POST | `/api/mobile/workbench/worktrees/push` | `routes/workbench.rs` | `git push` | requires-idempotency-key | same ledger/envelope |
| POST | `/api/mobile/workbench/worktrees/merge` | `routes/workbench.rs` | `git merge --no-ff` + cleanup | requires-idempotency-key | same ledger/envelope |
| POST | `/api/mobile/workbench/worktrees/remove` | `routes/workbench.rs` | `git worktree remove` + row delete | requires-idempotency-key | same ledger/envelope |
| POST | `/api/mobile/workbench/worktrees/mutation-operation` | `routes/workbench.rs` | none; reads durable ledger by `clientOperationId` | read-only | mobile ledger query for unknown envelope reconciliation |
| POST | `/api/mobile/workbench/git/commits` | `routes/workbench.rs` | none; `git log` read | read-only | — |
| POST | `/api/mobile/workbench/files/list-dir` | `routes/workbench.rs` | none | read-only | — |
| POST | `/api/mobile/workbench/files/info` | `routes/workbench.rs` | none | read-only | — |
| POST | `/api/mobile/workbench/files/open` | `routes/workbench.rs` | none | read-only | — |
| POST | `/api/mobile/workbench/files/save-text` | `routes/workbench.rs` | writes worktree-relative text file | naturally-idempotent | `baseHash` optimistic-lock guard |
| POST | `/api/mobile/workbench/sessions/list` | `routes/workbench.rs` | `restore_persisted_sessions` may recreate missing tmux windows | no-transport-retry | same subprocess/tmux spawn caveat as the desktop route; the registry guards against concurrent double-restore (Finding 5) but a transport replay still spawns real windows, so callers must not auto-retry |
| POST | `/api/mobile/workbench/sessions/create` | `routes/workbench.rs` | new tmux window / PTY + SQLite window row | requires-idempotency-key | no dedupe key yet; clients MUST NOT auto-retry |
| POST | `/api/mobile/workbench/sessions/replay` | `routes/workbench.rs` | none | read-only | — |
| POST | `/api/mobile/workbench/sessions/write` | `routes/workbench.rs` | writes bytes to PTY/tmux pane stdin | no-transport-retry | appended input; replay duplicates |
| POST | `/api/mobile/workbench/sessions/resize` | `routes/workbench.rs` | sets cols+rows, persists size | naturally-idempotent | same `(cols, rows)` is a no-op |
| POST | `/api/mobile/workbench/sessions/focus` | `routes/workbench.rs` | `tmux select-window` | naturally-idempotent | same target is a no-op |
| POST | `/api/mobile/workbench/sessions/focused` | `routes/workbench.rs` | none; `tmux display-message` query | read-only | — |
| POST | `/api/mobile/workbench/sessions/split-pane` | `routes/workbench.rs` | `tmux split-window` | requires-idempotency-key | no dedupe key yet; replay creates a second pane |
| POST | `/api/mobile/workbench/sessions/switch-pane` | `routes/workbench.rs` | `tmux select-pane` cycles to the next pane | no-transport-retry | relative cycle; replay lands on the wrong pane |
| POST | `/api/mobile/workbench/sessions/zoom-pane` | `routes/workbench.rs` | `tmux resize-pane -Z` guarded by zoom flag | naturally-idempotent | — |
| POST | `/api/mobile/workbench/sessions/close-pane` | `routes/workbench.rs` | `tmux kill-pane`/`kill-window` + row delete | no-transport-retry | destructive |
| POST | `/api/mobile/workbench/sessions/close` | `routes/workbench.rs` | `tmux kill-window`/`kill-session` + row delete | no-transport-retry | destructive |
| POST | `/api/mobile/workbench/prompt-optimizer/stream-to-session` | `routes/workbench.rs` | spawns Claude CLI, streams into terminal | no-transport-retry | paid, nondeterministic; replay duplicates |
| POST | `/api/mobile/workbench/browser/discover` | `routes/workbench.rs` | none | read-only | — |
| POST | `/api/mobile/workbench/browser/preview` | `routes/workbench.rs` | registers a previewId | requires-idempotency-key | new UUID per call; replay registers a stale entry |
| ANY | `/api/mobile/workbench/browser/proxy/:previewId/*path` | `routes/workbench.rs` | proxied pass-through; forwards arbitrary methods | no-transport-retry | method-agnostic proxy; replay can duplicate a non-idempotent upstream mutation |
| POST | `/api/orchestrator/tasks/create` | `routes/orchestrator.rs` | inserts authoritative task row + optional best-effort dispatch | requires-idempotency-key | non-empty `clientRequestId` required; `orchestrator_remote_task_create_requests` dedupes in one transaction (`create_remote_task_for_client_request`) |
| POST | `/api/orchestrator/tasks/complete-prompt` | `routes/orchestrator.rs` | invokes Claude CLI headless to draft title/goal/acceptance | no-transport-retry | paid, nondeterministic Orchestrator action; replay re-runs the LLM and may overwrite the user's edits |
| POST | `/api/orchestrator/tasks/list` | `routes/orchestrator.rs` | none | read-only | — |
| POST | `/api/orchestrator/task-views/list` | `routes/orchestrator.rs` | none | read-only | — |
| POST | `/api/orchestrator/task-views/create` | `routes/orchestrator.rs` | creates mobile task view (local project row, pending outbox, or remote mirror) | requires-idempotency-key | preserves `clientRequestId` via `create_orchestrator_task_view_for_http`; pending outbox keeps the same key |
| POST | `/api/orchestrator/outbox/retry` | `routes/orchestrator.rs` | failed outbox → pending on current device only | naturally-idempotent | failed-only SQL guard via `retry_failed_remote_outbox_item`; ownership checked against local remote shortcut |
| POST | `/api/orchestrator/outbox/discard` | `routes/orchestrator.rs` | failed outbox → discarded audit on current device only | naturally-idempotent | failed-only SQL guard via `discard_failed_remote_outbox_item`; ownership checked against local remote shortcut |
| POST | `/api/orchestrator/tasks/evidence` | `routes/orchestrator.rs` | none; reads evidence list | read-only | — |
| POST | `/api/orchestrator/tasks/review-diff` | `routes/orchestrator.rs` | none; reads bounded Human Review / Rework diff snapshot | read-only | capability-gated by `orchestrator.review-diff.v1`; body camelCase `{taskId}` only; base/head from task/worktree; rejects remote shortcuts; outside Human Review/Rework → conflict `review_diff_unavailable`; response includes `reviewDigest` + bounded files (≤200 / total patch ≤2 MiB / single ≤256 KiB; binary metadata only; repo-relative paths) |
| POST | `/api/mobile/orchestrator/tasks/review-diff` | `routes/orchestrator.rs` | none; remote-aware review diff for mobile browser | read-only | body camelCase `{projectId,taskId}`; reuses Tauri remote-aware helper; never exposes owner P2P base URL |
| POST | `/api/orchestrator/workflow-document/get` | `routes/orchestrator.rs` | none; reads WORKFLOW.md status/content/hash | read-only | capability-gated by `orchestrator.workflow-document.v1`; body camelCase `{projectId}`; rejects remote shortcuts; rejects symlink |
| POST | `/api/orchestrator/workflow-document/validate` | `routes/orchestrator.rs` | none; authoritative parse diagnostics | read-only | capability-gated by `orchestrator.workflow-document.v1`; body camelCase `{projectId,content}`; rejects remote shortcuts |
| POST | `/api/orchestrator/workflow-document/save` | `routes/orchestrator.rs` | CAS atomic write of WORKFLOW.md | no-transport-retry | capability-gated by `orchestrator.workflow-document.v1`; body camelCase `{projectId,expectedHash,content}`; expectedHash empty creates missing file only; drift → conflict `workflow_document_changed`; **does not dispatch and cannot enable/change delivery** |
| POST | `/api/mobile/orchestrator/workflow-document/get` | `routes/orchestrator.rs` | none; remote-aware WORKFLOW document read | read-only | body camelCase `{projectId}`; reuses Tauri remote-aware helper; never exposes owner P2P base URL |
| POST | `/api/mobile/orchestrator/workflow-document/validate` | `routes/orchestrator.rs` | none; remote-aware WORKFLOW validate | read-only | body camelCase `{projectId,content}`; reuses Tauri remote-aware helper |
| POST | `/api/mobile/orchestrator/workflow-document/save` | `routes/orchestrator.rs` | CAS save via remote-aware helper | no-transport-retry | body camelCase `{projectId,expectedHash,content}`; reuses Tauri helper; does not dispatch |
| POST | `/api/orchestrator/tasks/queue` | `routes/orchestrator.rs` | atomic Draft→Queued transition | no-transport-retry | Orchestrator lifecycle action; replay after timeout races the scheduler claim |
| POST | `/api/orchestrator/tasks/start` | `routes/orchestrator.rs` | moves task into scheduler path + best-effort dispatch | no-transport-retry | Orchestrator lifecycle action |
| POST | `/api/orchestrator/tasks/retry` | `routes/orchestrator.rs` | atomic Blocked→Queued transition | no-transport-retry | Orchestrator lifecycle action |
| POST | `/api/orchestrator/tasks/request-rework` | `routes/orchestrator.rs` | appends evidence + transitions to Rework/Idle | no-transport-retry | Orchestrator lifecycle action; replay adds duplicate repair evidence |
| POST | `/api/orchestrator/tasks/deliver-reviewed` | `routes/orchestrator.rs` | digest gate + Settings gate + full git delivery pipeline | no-transport-retry | body camelCase `{taskId, expectedReviewDigest?}`; owner always recollects before deliver when review-diff capable — non-empty `expectedReviewDigest` required, mismatch → conflict `review_diff_changed` (task stays Human Review, no Delivering/evidence); then Settings full-auto delivery gate; irreversible git writes; no transport retry |
| POST | `/api/orchestrator/tasks/abort` | `routes/orchestrator.rs` | sets authoritative task to Aborted | no-transport-retry | Orchestrator lifecycle action |
| POST | `/api/orchestrator/tasks/cancel` | `routes/orchestrator.rs` | transitions to Canceled/Idle, retains scene | no-transport-retry | Orchestrator lifecycle action |
| POST | `/api/orchestrator/projects/refresh` | `routes/orchestrator.rs` | best-effort `dispatch_once` | no-transport-retry | Orchestrator action; replay can race a concurrent scheduler tick |
| POST | `/api/orchestrator/runtime-snapshot` | `routes/orchestrator.rs` | none; reads owning-device local runtime snapshot | read-only | capability-gated by `orchestrator.runtime-snapshot.v1`; body snake_case `{project_id}` only; rejects remote shortcuts |
| POST | `/api/mobile/orchestrator/runtime-snapshot` | `routes/orchestrator.rs` | none; remote-aware runtime snapshot for mobile browser | read-only | body camelCase `{projectId}`; reuses Tauri four-state helper; never exposes owner P2P base URL |
| GET | `/api/orchestrator/config` | `routes/orchestrator.rs` | none | read-only | — |

## CC History paged sync contract (`cc-history.paged-sync.v1`)

New↔new peers use the three routes above when health advertises
`cc-history.paged-sync.v1`. The token and routes ship in the same build.

| Topic | Contract |
| --- | --- |
| Capability gate | Client reads `/api/health` first. **Only** when `supports("cc-history.paged-sync.v1")` is true may it call manifest-page / items / push-batch. |
| v0 / mixed-version fallback | Peers without the token keep using legacy `POST /api/cc-history/sync/pull` and `/push`. New servers still mount those legacy routes. Clients must not probe paged paths and treat 404 as capability. |
| Cursor opacity | `cursor` / `next_cursor` are opaque server tokens (base64url of `{v:1,last_id}`). Clients pass them through unchanged and must not parse, invent, or resume across process restarts. |
| Limits | Manifest default 256 / max 512; items & push-batch max **128** rows; single `content` ≤ **1 MiB** UTF-8; single request/response estimate ≤ **8 MiB**; single id ≤ **256** UTF-8 bytes; blank/duplicate ids rejected. |
| Error codes | `400 cc_history.invalid_cursor`; `413 cc_history.batch_too_large` (`retryable=false`); `422 cc_history.item_too_large`; validation → `400` with stable domain action. |
| Partial batch | push-batch is **all-or-nothing** in one DB transaction. There is no partial `accepted` subset for a failed batch. |
| Retry / restart-from-zero | Interrupted paged rounds do **not** persist remote cursors. The next sync restarts summary exchange from the first page. Vector-clock merge + upsert make replaying whole batches safe. On `batch_too_large` the client may bisect the id list down to one; a single `item_too_large` ends that round for the offending item (no silent skip). |
| Metrics privacy | Process-local `RuntimeMetrics` may record fixed names such as `cc_history.sync_batch.*` / `cc_history.sync_round_ms` and orchestrator claim counters. Metrics stay in-process / sanitized tracing only — **no** telemetry upload, and **never** content, paths, project/device names, host, SQL, or credentials. |
| SQLite pool | Production remains `max_connections(1)` with WAL and `busy_timeout=5s` unless a separate Task 8 load-gate commit documents evidence to raise it to **2** (never 3+). |

## Content sync v2 contract (`sync.manifest.v2`)

Covers **Prompt**, **SSH target**, and **Scratchpad** only (not CC History). New↔new
peers use the twelve v2 routes when health advertises `sync.manifest.v2`
(`manifest-page` / `items` / `push-batch` / `ack-delete-epoch` × 3 domains). The token
ships with transactional `apply_*_merge_batch` + `sync_request_ledger`.

| Topic | Contract |
| --- | --- |
| Capability gate | Client reads `/api/health` first. **Only** when `supports("sync.manifest.v2")` is true may it call any of the three domains' `manifest-page` / `items` / `push-batch` / `ack-delete-epoch`. |
| Mixed-version | **v2 client ↔ legacy server**: use typed legacy `pull`/`push` for that domain; transport/HTTP/JSON failures remain typed (`Unreachable` / `ProtocolError` / …) and **must not** be fabricated into empty success. **Legacy client ↔ v2 server**: legacy routes stay mounted; server does not require the capability for legacy paths. Do not probe v2 paths and treat 404 as capability. |
| Equal manifests | After both complete sorted manifests are available, exact equality (id + vector_clock + content_hash + deleted) yields `unchanged=N` with **zero** body fetch and **zero** content push. Watermark ack (if needed) uses dedicated `ack-delete-epoch`, never an empty push-batch. |
| Cursor opacity | `cursor` / `next_cursor` are opaque server keyset tokens. Clients must stream pages until `next_cursor=None` before classifying remote-only items. Incomplete streams → `ProtocolError` / incomplete_manifest and **must not** advance `acked_delete_epoch`. |
| Limits | Manifest-page: max **500** items / **1 MiB** estimate. Items & push-batch: max **100** items / **4 MiB** estimate. Single content >1 MiB → 422 `*.item_too_large`. |
| push-batch fields | Required non-empty `client_request_id`. Optional `claimed_device_id` is a convergence **label, not authentication**. Optional `acked_delete_epoch` only after a complete remote manifest stream + successful apply; otherwise omit/`null`. Last **content** push-batch may still carry ack; empty content push solely for ack is forbidden. |
| ack-delete-epoch | Body `{claimed_device_id,acked_delete_epoch}`; advances peer/domain watermark via `SyncWatermarkRepo::advance_ack` (MAX-only, naturally idempotent). No items upsert and no content ledger claim. Empty/blank `claimed_device_id` → 400 Validation. |
| Ledger / idempotency | Server ledger key `UNIQUE(claimed_device_id, domain, client_request_id)` stores payload hash + `{accepted}` outcome. Same key + same hash → return recorded outcome without re-apply. Same key + different hash → **409 conflict**. Apply path is one SQLite transaction via `apply_*_merge_batch`. |
| Error codes | `400 *.invalid_cursor` / empty `client_request_id` / empty `claimed_device_id` on ack; `413 *.batch_too_large` (`retryable=false`); `422 *.item_too_large`; `409` ledger payload conflict. |
| Domain outcomes | Aggregated as `Succeeded{pulled,pushed,unchanged}` / `Partial` / `Unreachable` / `ProtocolError` / `ResourceLimit`. Only full-device `Succeeded` increments `succeeded_devices` / `synced`. |
| Delete ack / floors | Incomplete manifest or failed apply must not ack. When content batches are empty, client calls dedicated `ack-delete-epoch`; when content exists, last push-batch may still carry ack. Tombstone GC (age ≥30d and all peers active in 90d have acked the epoch) is a server/local helper concern; floors reject offline-peer resurrection. |

## Transfer lifecycle and recovery contract (`transfer.resume.v1`)

Recovery sits **above** the existing init/chunk/complete/status wire. No new LAN
routes are introduced for retry/resume/operation-query; those are sender-owner
Tauri/sidecar commands (GuiClient proxies via loopback control). Open/Reveal and
send/retry/resume/get-operation/cancel are **local control plane**
(`POST /api/backend/control/transfer/*`), never advertised on LAN.

| Topic | Contract |
| --- | --- |
| Capability | `transfer.resume.v1` is advertised in `server_protocol_info()` / `/api/health` and mDNS caps. Sender may resume **only** when the peer `supports("transfer.resume.v1")`. UI gates 「继续传输」 on peer capability (mDNS hint fail-closed); backend still re-checks health. Token ships with sender resume claim + stable protocol id reuse in the same build. |
| Old-peer fallback | Peer without `transfer.resume.v1` → UI/API fall back to **full retry** only. Do **not** fake continue-from-offset. Chunk resume_offset from init still works for mid-transfer reconnects on the same protocol id; the recovery *command* surface is retry. |
| `clientOperationId` vs request id | `clientOperationId` is the **sender-owned** durable idempotency key (`UNIQUE(client_operation_id)` + canonical payload hash). Hash covers operation kind, logical transfer/source identity, peer, and expected protocol id. **Retry** hashes use an empty protocol placeholder (must not fold a per-call random UUID); Fresh claim then binds attempt/protocol. **Resume** hashes include the stable parent protocol id. Same id + same hash → Replay (terminal Replay → `operation_terminal` conflict requiring new op id; non-terminal orphan → re-spawn); same id + different hash → typed `operationIdConflict`. `get_transfer_operation` queries **only** this key. `X-CC-Request-Id` / per-invoke request ids are **trace-only** and never used as idempotency keys. |
| Owner control proxy | GuiClient must proxy send/retry/resume/get-operation/cancel (and prepare-open) through loopback control to HeadlessOwner; registry/spawn lives only on the owner process. |
| Stable resume protocol id | Wire `transfer_id` **is** `protocolTransferId`. Resume reuses it so receiver tmp/checkpoint/finalize journal hit the same row. Full retry mints a new protocol id only on Fresh claim (bound to the claim attempt). Receiver durability / finalize journal / status are keyed **only** by protocol id — receivers do **not** store `clientOperationId`. |
| Source fingerprint | Fixed shape `{size, mtimeNsOrNull, sha256}`. SHA is computed in a blocking worker on the opened file handle; size/mtime rechecked before spawn. mtime unavailable → re-validate size+SHA. Any mismatch → reject resume / typed `source_changed`; never finalize on TOCTOU replace. Receiver must re-verify SHA before place; hash mismatch rejects and does not promote. |
| Lost final ACK | After complete response loss, sender reads `GET /api/transfer/status/:protocolTransferId`. If receiver already `completed`, sender commits local task+operation outcome in one transaction. Must **not** re-enter destructive finalize/place. Still pending/unreachable → park as finalizing/uncertain; UI shows reconciling and blocks blind retry. |
| Open / Reveal | Same-device **desktop GUI only** for `direction=Receive` + `completed`. Sidecar validates path (no-follow ordinary file) via loopback `prepare-open` and returns `LocalTransferOpenTarget`; GUI runs `plugin-opener` `openPath` / `revealItemInDir`. P2P and mobile return `unsupported`; never expose absolute paths over LAN. |
| 1 GiB dual-host | Real disconnect/restart/resume/SHA on two physical hosts is deferred L3 (`L3-DUAL-HOST-LAN-001`, **NOT VERIFIED**). L0/L2 smoke must not claim that surface. |

## Notes on the mandatory classifications

- **Server-enforced idempotency keys today.** (1) Orchestrator create:
  `POST /api/orchestrator/tasks/create` rejects a missing/blank key and
  deduplicates via `orchestrator_remote_task_create_requests` in the same
  transaction (`create_remote_task_for_client_request`). Mobile
  `task-views/create` preserves the same key end-to-end through the commands
  layer and pending outbox. (2) Content sync v2 push-batch:
  `client_request_id` + optional `claimed_device_id` + domain via
  `sync_request_ledger` (same key/hash → recorded outcome; same key/different
  hash → 409). (3) Workbench Git mutation: `clientOperationId` +
  `workbench_mutation_operations` ledger. (4) **Transfer sender recovery**:
  sender-local `clientOperationId` + payload hash ledger (not a LAN route key);
  wire reuses stable `protocolTransferId` as `transfer_id` for resume.
- **`requires-idempotency-key` rows without a key yet.** Worktree create,
  terminal session create, split-pane, Claude resume, file/dir create,
  browser-preview create, **and transfer/init** have no *receiver* transport
  dedupe key beyond the client-supplied `transfer_id`. Resume **must** reuse
  that id; a full retry that mints a fresh id is intentional and creates a new
  tmp. Transport MUST NOT auto-retry init with a newly minted id.
- **`naturally-idempotent` rows are verified by code.** Each "Key / guard" cell
  cites the function or mechanism that makes replay converge:
  - legacy sync/cc-history/ssh-target/scratchpad push — per-row vector-clock
    merge + conditional `bulk_upsert`;
  - content sync v2 push-batch — `sync_request_ledger` +
    `apply_*_merge_batch` single transaction (same key/hash → recorded outcome);
  - claude_md push — sender pushes its own merged version; re-apply is a no-op;
  - save-text — `baseHash` optimistic-lock guard;
  - projects/open — same canonical path reuses the same project id;
  - resize/focus/zoom-pane/rename — fixed-target tmux operations guarded by
    current state where relevant.
- **`no-transport-retry` rows.** terminal write, git commit/push/merge/remove,
  file rename/delete, session close/close-pane, **switch-pane** (relative
  `select-pane` cycle — replay lands on the wrong pane), prompt-optimizer
  streaming, the **browser proxy** (`any(...)` method-agnostic pass-through —
  can replay a non-idempotent upstream POST/PUT/DELETE), Orchestrator lifecycle
  actions, project refresh and the local backend stop control all mutate
  irreversible or externally observable state, or forward to something that does.
  The transport layer must not auto-replay them.

## Adding a new route (protocol change checklist)

Every new `POST`/`PUT`/`DELETE`/`PATCH` route (and any new `GET` that introduces
a wire-format change) must, in the same change:

1. **Capability constant** — add a `CAPABILITY_*_V1` constant in
   `src-tauri/src/net/protocol.rs` if the route depends on a new wire shape or
   behavior, and add it to `server_protocol_info()`.
2. **Health declaration** — confirm `GET /api/health` advertises the capability
   so peers can gate the client call.
3. **Legacy contract test** — keep the route tolerable for v0 peers: unknown
   fields ignored, missing fields defaulted, and the client degrades gracefully
   when `PeerProtocolInfo::supports(...)` is false.
4. **Typed error code** — return failures through the `P2pError` envelope with a
   stable `<domain>.<action>` code so clients can branch on typed errors.
5. **Request ID** — the global `request_id_middleware` already attaches
   `X-CC-Request-Id`; the handler MUST log and propagate the
   `P2pRequestContext` it receives.
6. **Inventory row** — add the route to the table above with method, owner,
   side effect, retry class and key/guard.
7. **Explicit retry policy** — pick exactly one retry class and document *why*
   in the "Key / guard" cell. New mutating side effects default to
   `no-transport-retry` unless the implementation is demonstrably replay-safe
   (cite the function) or a new idempotency key is added in the same change.

After editing, run `node scripts/check-p2p-route-inventory.mjs` — it exits
non-zero if the router and the table drift apart.
