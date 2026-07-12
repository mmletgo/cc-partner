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
advertised capability is `errors.envelope.v1`.

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
- This is the only v1 capability advertised today. Future capabilities (Runtime
  notifications, Inbox, …) will ship as **independent** tokens alongside their
  own routes and must not reuse this token to mean "new routes supported".

The existing capability gate (`peer_client::require_capability`) is therefore a
**format** gate, not an authorization gate: it lets a caller avoid sending a
v1-envelope-dependent request to a v0 peer that would only return a legacy
error. It does not restrict which routes a peer may call.

## Route inventory

The "Path" column mirrors the literal string passed to axum `.route(...)`.
Dynamic segments (`:id`, `:previewId`, `*path`) are documented identically to
the router so the inventory check matches exactly.

| Method | Path | Owner | Side effect | Retry class | Key / guard |
| --- | --- | --- | --- | --- | --- |
| GET | `/api/health` | `routes/health.rs` | none | read-only | — |
| POST | `/api/backend/control/stop` | `http_server.rs` | signals local `serve` shutdown via control-token gate | no-transport-retry | `controlToken` must match control file; retry after timeout can hit a recycled port |
| GET | `/api/mobile/access-info` | `routes/mobile.rs` | none | read-only | — |
| POST | `/api/sync/pull` | `routes/sync.rs` | none; returns rows the caller is missing | read-only | vector-clock comparison only reads local DB |
| POST | `/api/sync/push` | `routes/sync.rs` | upserts prompt rows after vector-clock merge | naturally-idempotent | `sync_push_impl` re-merges each row; `bulk_upsert` with merged clock converges on replay |
| POST | `/api/sync/claude_md/pull` | `routes/claude_md_sync.rs` | none; returns the singleton row | read-only | — |
| POST | `/api/sync/claude_md/push` | `routes/claude_md_sync.rs` | overwrites singleton + `~/.claude/CLAUDE.md` | naturally-idempotent | sender only pushes its own already-merged version; re-applying the same row + `write_file_if_changed` is a no-op |
| POST | `/api/transfer/init` | `routes/transfer.rs` | creates `.{transfer_id}.tmp` + receive registry entry | requires-idempotency-key | `transfer_id` is client-supplied and the tmp file is keyed by it, but the server does not enforce that a transport replay reuses the same `transfer_id`; a retry that mints a new id leaks a tmp file + registry entry. Clients MUST reuse the same `transfer_id` (the de-facto idempotency key) or MUST NOT auto-retry |
| POST | `/api/transfer/chunk/:id` | `routes/transfer.rs` | writes bytes at offset, finalizes when complete | no-transport-retry | the final chunk triggers `finalize_transfer` which renames the tmp file into its final path and removes the registry entry; a transport-layer replay of the same final-chunk request previously hit a missing registry entry and returned `success:false`. The receiver now guards finalize with a per-`transfer_id` singleflight lock + a short-lived terminal tombstone, so a duplicate final chunk returns the first finalize's result — but middle chunks still mutate the tmp file at arbitrary offsets and there is no per-offset dedupe, so transport-layer retries must be disabled; callers surface the failure and let the user re-initiate |
| GET | `/api/transfer/status/:id` | `routes/transfer.rs` | none | read-only | — |
| POST | `/api/cc-history/sync/pull` | `routes/cc_history.rs` | none | read-only | — |
| POST | `/api/cc-history/sync/push` | `routes/cc_history.rs` | upserts CC history rows after merge | naturally-idempotent | per-row `merge_cc_history` + `bulk_upsert`; replay converges |
| POST | `/api/ssh-target/sync/pull` | `routes/ssh_target_sync.rs` | none | read-only | — |
| POST | `/api/ssh-target/sync/push` | `routes/ssh_target_sync.rs` | upserts SSH target rows after merge | naturally-idempotent | per-row `merge_ssh_target` + `bulk_upsert`; replay converges |
| POST | `/api/scratchpad/sync/pull` | `routes/scratchpad_sync.rs` | none | read-only | — |
| POST | `/api/scratchpad/sync/push` | `routes/scratchpad_sync.rs` | upserts scratchpad pages after merge | naturally-idempotent | per-row `merge_scratchpad` + `bulk_upsert`; replay converges |
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
| POST | `/api/workbench/worktrees/commit` | `routes/workbench.rs` | `git add -A` + `git commit` | no-transport-retry | replay can create a second empty commit or rerun the Claude message generator |
| POST | `/api/workbench/worktrees/push` | `routes/workbench.rs` | `git push` of the task branch | no-transport-retry | network replay after a timeout can race the remote and surface a spurious failure |
| POST | `/api/workbench/worktrees/merge` | `routes/workbench.rs` | `git merge --no-ff`, conflict resolution, branch delete | no-transport-retry | irreversible; replay after partial completion can corrupt the merge |
| POST | `/api/workbench/worktrees/remove` | `routes/workbench.rs` | `git worktree remove` + row delete | no-transport-retry | destructive |
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
| GET | `/api/workbench/events` | `routes/workbench.rs` | none; NDJSON event stream | read-only | — |
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
| POST | `/api/mobile/workbench/worktrees/commit` | `routes/workbench.rs` | `git add -A` + `git commit` | no-transport-retry | — |
| POST | `/api/mobile/workbench/worktrees/push` | `routes/workbench.rs` | `git push` | no-transport-retry | — |
| POST | `/api/mobile/workbench/worktrees/merge` | `routes/workbench.rs` | `git merge --no-ff` + cleanup | no-transport-retry | — |
| POST | `/api/mobile/workbench/worktrees/remove` | `routes/workbench.rs` | `git worktree remove` + row delete | no-transport-retry | destructive |
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
| POST | `/api/orchestrator/tasks/queue` | `routes/orchestrator.rs` | atomic Draft→Queued transition | no-transport-retry | Orchestrator lifecycle action; replay after timeout races the scheduler claim |
| POST | `/api/orchestrator/tasks/start` | `routes/orchestrator.rs` | moves task into scheduler path + best-effort dispatch | no-transport-retry | Orchestrator lifecycle action |
| POST | `/api/orchestrator/tasks/retry` | `routes/orchestrator.rs` | atomic Blocked→Queued transition | no-transport-retry | Orchestrator lifecycle action |
| POST | `/api/orchestrator/tasks/request-rework` | `routes/orchestrator.rs` | appends evidence + transitions to Rework/Idle | no-transport-retry | Orchestrator lifecycle action; replay adds duplicate repair evidence |
| POST | `/api/orchestrator/tasks/deliver-reviewed` | `routes/orchestrator.rs` | Settings gate + full git delivery pipeline | no-transport-retry | Orchestrator lifecycle action driving irreversible git writes |
| POST | `/api/orchestrator/tasks/abort` | `routes/orchestrator.rs` | sets authoritative task to Aborted | no-transport-retry | Orchestrator lifecycle action |
| POST | `/api/orchestrator/tasks/cancel` | `routes/orchestrator.rs` | transitions to Canceled/Idle, retains scene | no-transport-retry | Orchestrator lifecycle action |
| POST | `/api/orchestrator/projects/refresh` | `routes/orchestrator.rs` | best-effort `dispatch_once` | no-transport-retry | Orchestrator action; replay can race a concurrent scheduler tick |
| POST | `/api/orchestrator/runtime-snapshot` | `routes/orchestrator.rs` | none; reads owning-device local runtime snapshot | read-only | capability-gated by `orchestrator.runtime-snapshot.v1`; body snake_case `{project_id}` only; rejects remote shortcuts |
| POST | `/api/mobile/orchestrator/runtime-snapshot` | `routes/orchestrator.rs` | none; remote-aware runtime snapshot for mobile browser | read-only | body camelCase `{projectId}`; reuses Tauri four-state helper; never exposes owner P2P base URL |
| GET | `/api/orchestrator/config` | `routes/orchestrator.rs` | none | read-only | — |

## Notes on the mandatory classifications

- **Orchestrator create keeps `clientRequestId`.** `POST /api/orchestrator/tasks/create`
  already rejects a missing/blank key and deduplicates via
  `orchestrator_remote_task_create_requests` in the same transaction
  (`create_remote_task_for_client_request`). Mobile `task-views/create` preserves
  the same key end-to-end through the commands layer and pending outbox. Both
  rows above are the only currently-implemented *server-enforced* idempotency keys.
- **`requires-idempotency-key` rows without a key yet.** Worktree create,
  terminal session create, split-pane, Claude resume, file/dir create,
  browser-preview create, **and transfer/init** have no server-side dedupe today.
  `transfer/init` keys its tmp file by the client-supplied `transfer_id`, but the
  server does not enforce that a transport replay reuses the same id — a retry
  that mints a fresh `transfer_id` leaks a tmp file. Until each of these gets its
  own server-enforced key, the client transport MUST NOT auto-retry them: surface
  the timeout and let the user re-trigger explicitly (for transfer/init the
  client MAY auto-retry only if it deterministically reuses the original
  `transfer_id`).
- **`naturally-idempotent` rows are verified by code.** Each "Key / guard" cell
  cites the function or mechanism that makes replay converge:
  - sync/cc-history/ssh-target/scratchpad push — per-row vector-clock merge +
    conditional `bulk_upsert`;
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
