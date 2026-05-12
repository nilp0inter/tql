# PLAN.md

The implementation plan for `tql`, evolving leg by leg. Each leg is small enough
to finish in one session.

## Status

Leg 58c done — per-tracker notify override (notify.rhai / notify.hbs in
the tracker bundle) wired through the drainer with tracker → global →
embedded → minimal fallback. Next pending: Leg 58d (illustrative
example tracker override + fixture).

## Old Status notes

- DESIGN.md: complete (authored by user).
- Implementation: not started — no Cargo project yet.

## Legs

### Leg 1 — Scaffold the Cargo project and CLI dispatch (DONE 2026-05-11)

Goal: a buildable `tql` binary with `clap` v4 derive top-level dispatch covering
every subcommand listed in DESIGN.md §7. Each subcommand is a stub that prints
"not yet implemented" and exits 0 (or non-zero for doctor/test as appropriate
later). No business logic yet.

Deliverables:
- `Cargo.toml` with the binary target and minimal dependencies (`clap` only).
- `src/main.rs` with the top-level `Cli` enum.
- `src/cmd/mod.rs` plus a stub file per subcommand (`mcp`, `api`, `cli`,
  `post_process`, `reconcile`, `link_add`, `link_remove`, `sidecar_show`,
  `doctor`, `test`, `reload`).
- `cargo build` succeeds via `nix shell nixpkgs#cargo nixpkgs#rustc`.
- `tql --help` lists all subcommands.

Out of scope for this leg: config, qBittorrent client, Rhai scripting, REST,
MCP, post-process logic. Those are their own legs.

### Leg 2 — Config loading (config.rs) (DONE 2026-05-11)

Define the `Config` struct matching DESIGN.md §11; load from TOML via `figment`
with env overrides; `tql doctor` becomes the first real consumer (parse + report).
Adds `figment`, `serde`, `toml`.

Outcome: `src/config.rs` with full §11 struct tree, env override via
`TQL_FOO__BAR` (double-underscore section split), search order
`$TQL_CONFIG` → `$XDG_CONFIG_HOME/tql/config.toml` → `/etc/tql/config.toml`,
overridable with `tql doctor --config <path>`. `doctor` now prints a summary
and exits non-zero on parse failure; deeper checks deferred to Leg 16.

### Leg 3 — Path sanitization & link-tag validation (DONE 2026-05-11)

Pure functions per DESIGN.md §5 and §10 in `src/paths.rs`. Property tests
included via `proptest` (idempotence, size bound, forbidden-char absence,
containment under `<library_root>/<category>/`). No I/O.

Outcome: `sanitize_component`, `parse_link_tag`, `resolve_link_site` plus
`SanitizeOpts`, `LinkTagError`, `LinkTag`. Constants for the soft caps and
PATH_MAX. Adds `unicode-normalization`, dev-dep `proptest`. 26 tests pass.

### Leg 4 — Sidecar read/write (sidecar.rs) (DONE 2026-05-11)

Atomic write under `flock`, schema_version=1, round-trip tests. Adds `fs2`.

Outcome: `src/sidecar.rs` with `Sidecar`, `LinkSite`, `Origin` types and
`read`/`write`/`sidecar_path` helpers. Exclusive `flock` for write, shared
for read (held on adjacent `.<name>.lock`). Write-temp-then-rename + `fsync`.
Missing sidecar → `Ok(None)`. Malformed → `SidecarError::Parse`. Unknown
`schema_version` → `SidecarError::UnsupportedSchema`. Adds `serde_json`, `fs2`.
8 tests pass (34/34 total).

### Leg 5 — Linking primitives (linking.rs) (DONE 2026-05-11)

`link(2)` / reflink / atomic rename. Single-file and directory cases.

Outcome: `src/linking.rs` with `link_to_site`, `unlink_site`, `LinkStrategy`
(`Hardlink|Reflink|ReflinkOrHardlink`), `LinkOpts`, `LinkOutcome`,
`LinkError`, `UnlinkError`. Build-at-sibling-temp + atomic `rename(2)`;
recursive tree replication for multi-file torrents; symlinks reproduced as
symlinks; `EXDEV` surfaces as `CrossDevice` (no copy fallback). Idempotent
re-run via inode comparison (file) or recursive structural+inode match
(directory). `unlink_site` prunes newly-empty parents up to a stop boundary
(`<library_root>/<category>/`). Adds `reflink-copy`. 9 tests pass (43/43).

### Leg 6 — qBittorrent WebUI client (qbit/)

Login, addTorrent, getTorrents. Mock server in tests. Split into:

- **Leg 6a** — Module skeleton + `login`. (DONE 2026-05-11) Adds `tokio`,
  `reqwest` (rustls + cookies + json + multipart). `Client` holds cookie
  jar; `login` POSTs form-urlencoded `username/password` to
  `/api/v2/auth/login` and inspects the `Ok.`/`Fails.` body (qBittorrent
  uses HTTP 200 for both outcomes). 403 → `Banned`. Tests use a hand-rolled
  `TcpListener` mock (no extra dep). 49/49 tests pass.
- **Leg 6b** — `add_torrent` (DONE 2026-05-11). Multipart POST to
  `/api/v2/torrents/add`. Supports both `TorrentSource::File { filename,
  bytes }` and `TorrentSource::Url(_)` (magnet or http(s) torrent URL).
  `AddTorrentParams` carries optional `category`, `tags` (Vec joined as CSV
  in a single `tags` field), `paused`, `auto_tmm`, `savepath`. New error
  variants `AddFailed { status, body }` (HTTP non-2xx *or* HTTP 200 body
  != `Ok.`) and `NothingToAdd` (empty url). 5 tests: file-success +
  metadata assertions, url-success, `Fails.` body, HTTP 415, empty-url.
  54/54 green.
- **Leg 6c** — `torrents_info` (DONE 2026-05-11). `Client::torrents_info(&query)`
  → `Vec<TorrentInfo>` (fields: `hash`, `name`, `category: Option<String>`,
  `tags: Vec<String>`, `save_path`). `TorrentsInfoQuery` carries optional
  `hashes` (joined by `|`), `category`, `tag`. Empty category in the wire
  payload normalizes to `None`; CSV `tags` split into `Vec`. Non-2xx →
  `AddFailed` (reused). 3 tests: list parse + normalization, filter query
  serialization (asserts `%7C` separator), HTTP 403. 57/57 green.

### Leg 7 — Tracker manifest parsing (scripting/manifest.rs) (DONE 2026-05-11)

Parse `manifest.toml`; build the per-tracker Input schema. No Rhai yet.

Outcome: `src/scripting/{mod,manifest}.rs`. Public types `Manifest`,
`InputField`, `FieldType` (`String|Int|Bool|Array|Enum|MapStringString`),
`ManifestError`. `parse(&str)` and `load(&Path)` entry points. Full §12
example parses. Validation: name charset/length, non-empty description,
duplicate field names, unknown types, nested `array<...>` recursion,
`enum<a,b,c>` variant list, default-value type-check (incl. enum-must-be-
variant, map-of-string), `min/max_items` only on arrays + ordering,
`cli_separator` only on arrays, identifier-shaped field names. 15 new tests;
72/72 total green. No new deps (toml + serde already in tree).

### Leg 8 — Rhai sandbox + classify execution (scripting/)

Engine setup per DESIGN.md §12 sandboxing rules. Run `classify(input)`,
collect `ClassifyOutput`, validate against §5 producer rules. Split into:

- **Leg 8a** — Sandbox engine + `run_classify` + output validation.
  (DONE 2026-05-11) Adds `rhai` v1 with `default-features=false` +
  `std,sync`. `src/scripting/{sandbox,types,host}.rs`. `build_engine`
  starts from `new_raw`, re-enables basic Array/String/Map/Math/Logic
  packages, sets op/string/array/map/call/expr limits, disables the
  `eval` symbol, and registers pure helpers `sanitize` + `slug`.
  `run_classify` calls the script's `classify(input)`, requires a Map
  return with `link_tags`/`info_tags`/`warnings` arrays-of-strings,
  enforces §5 producer rules per tag (via `parse_link_tag` with the
  canonical category), folds soft caps into warnings, and hard-caps
  arrays. Caller passes a pre-built Rhai `Map` as input; manifest-driven
  marshaling lands in 8b. 89/89 tests green.
- **Leg 8b** — Marshal manifest-typed inputs into a Rhai `Map` (DONE
  2026-05-11). `src/scripting/input.rs::marshal_input(&Manifest, &Json)
  -> Result<rhai::Map, InputError>`. Validates: required-or-default,
  per-field type, array `min_items`/`max_items`, enum-variant
  membership, `map<string,string>` value types, unknown top-level
  fields. Optional+no-default fields are *omitted* from the map (script
  reads `input.foo == ()`). 12 tests added incl. an integration that
  feeds the marshaled map into `run_classify`. 101/101 green.

### Leg 9 — Tracker registry + `tql test` (fixtures runner)

Discover all trackers, run all fixtures. Split:

- **Leg 9a** — Tracker registry (DONE 2026-05-11). `src/scripting/registry.rs`
  with `Registry`, `Tracker { manifest, script: Arc<AST>, dir }`, `load_dir`,
  `LoadReport { registry, failures }`, `TrackerLoadError` (Missing*, Io,
  Manifest, Compile, DuplicateName), `RegistryError` (RootMissing, RootNotDir,
  Io). Per-tracker errors aggregated, never fatal; top-level errors only fire
  when `<trackers_root>` itself is unusable. `BTreeMap` keyed by manifest
  `name` for stable iteration. Hidden dirs and non-dir entries at root
  silently skipped. 13 new tests; 114/114 green. No new deps (tiny
  in-test `tempdir_lite` helper avoids dragging `tempfile` into the tree).
- **Leg 9b** — Fixture runner + `tql test [tracker]` CLI command (DONE
  2026-05-11). `src/scripting/fixtures.rs` with `Fixture`,
  `ExpectedOutput`, `FixtureFailure { tracker, fixture, kind }` where
  `kind = Io|Parse|Input|Classify|Mismatch`. `discover(tracker_dir)`
  enumerates `fixtures/*.toml` (sorted, hidden + non-TOML skipped).
  `run_all(engine, registry, only)` orchestrates discover →
  `marshal_input` → `run_classify` → equality. `cmd/test.rs` loads
  config (with `--config` flag), runs everything, reports a summary,
  exits 1 on any failure (deviates from "first failure" — see
  EXECUTION.md). Unknown tracker filter is an error. New in-tree
  example tracker `trackers/example/` with two fixtures exercises it
  end-to-end (`1 tracker, 2 fixtures, 2 passed`). 121/121 tests green
  (+7). No new deps.

### Leg 10 — `tql post-process`

The qBittorrent hook. Wires sidecar + linking + validation together.

- **Leg 10a** — Core post-process pipeline (DONE 2026-05-11). `src/cmd/post_process.rs`
  rewritten from stub to real implementation. `Args` gains an optional
  `--config` flag for tests/replays. `process(&Args) -> Outcome` is the
  testable core; `run` wraps it and always returns `Ok(())` per §7.
  Pipeline: load config → flock on `.metadata/.<hash>.json.pp.lock` (30 s
  wait, distinct path from sidecar's own lock to avoid same-OFD deadlock) →
  category sanity check → parse + validate `link:` tags via §5/§10 →
  diff vs existing sidecar → `link_to_site` / `unlink_site` per site →
  write new sidecar (atomic, under its own flock). Stale removal happens
  *after* additions so a clobber-free re-tagging keeps the inode alive
  during the swap. Soft caps fold to warnings; hard caps were already
  enforced upstream. No new deps (manual Hinnant epoch→ISO 8601 helper
  rather than chrono). 11 new tests; 132/132 green. Notifications + media
  refresh (§7 steps 8–9) deferred to Leg 15.

### Leg 11 — `tql reconcile` (DONE 2026-05-11)

The safety net.

Outcome: `src/cmd/reconcile.rs` rewritten from stub. `post_process` grew a
reusable `process_with_cfg(args, cfg, opts)` core (old `process` becomes a
thin loader wrapper) plus `Outcome::Planned { hash, adds, removes, warnings }`
for `--dry-run`. `TorrentInfo` extended with `content_path` + `size`
(`#[serde(default)]` for back-compat). Reconcile flow: load config →
`[qbittorrent]` required → tokio current-thread runtime → login →
`torrents_info(--torrent? --category?)` → per-torrent
`post_process::process_with_cfg` (sequential; per-hash flock lives inside).
Summary `{ total, ok, planned, aborted, warnings }`. Exit 1 on any abort
or transport error, 0 otherwise. Torrents without a category are skipped
with a warning rather than aborting the whole run. Bounded parallelism
(`[reconcile] parallelism`) deferred to a polish sub-leg. 135/135 tests
green (+3 in `cmd::reconcile::tests`). No new deps.

### Leg 12 — Transports: CLI subcommands per tracker

Dynamic clap from manifests. Split:

- **Leg 12a** — Dynamic command + classification preview (DONE 2026-05-11).
  `src/cmd/cli.rs` rewritten. `tql cli` (no args) lists registered
  trackers; `tql cli <tracker> [--field …] <SOURCE>` builds a
  `clap::Command` from the manifest, parses the user's flags into JSON,
  marshals via `marshal_input`, runs `classify` against the sandbox, and
  prints a preview block (input echo + link/info tags + warnings).
  `--help` is forwarded into the dynamic command (outer `Args` sets
  `disable_help_flag = true`). Source kind detection
  (`magnet:`/`http(s)://`/file) implemented but not yet consumed. Manifest
  strings are `Box::leak`'d into `&'static str` to satisfy clap's
  `Into<Str>` bound. 10 new tests; 145/145 green. qBittorrent add wiring
  (fetch source + POST `/torrents/add`) deferred to Leg 12b.
- **Leg 12b** — Wire to qBittorrent (DONE 2026-05-11). `dispatch` now
  takes `&Config` + `dry_run`; default mode builds a `TorrentSource`
  (file→bytes, magnet/url→passthrough), logs in to qBittorrent, calls
  `add_torrent` with `category = canonical_category` and `tags =
  link_tags ++ info_tags`, and prints a pretty-JSON acknowledgment
  `{ ok, tracker, category, info_hash, link_tags, info_tags, warnings,
  source: { kind, value } }`. `info_hash` is extracted from
  `xt=urn:btih:` on magnets and `null` for file/URL sources (bencode
  parsing deferred). `--dry-run` preserves the Leg 12a preview path.
  6 new helper tests; 151/151 green. No new deps.

- **Leg 12c** — Compute info_hash for file sources (DONE 2026-05-11).
  New `src/torrent.rs` with a minimal bencode scanner that locates the
  byte range of the top-level `info` dict (no re-encoding — BEP 3 hashes
  the wire bytes verbatim). `compute_info_hash(&[u8]) -> Result<String,
  BencodeError>` SHA1s that range and returns lowercase hex. `cli::build_ack`
  takes an optional `torrent_bytes: Option<&[u8]>` and fills `info_hash`
  for file sources too. Failure to parse degrades gracefully to `null` (we
  still want the qBittorrent add to succeed). Adds `sha1 = "0.10"`. 7 new
  tests (6 in `torrent`, 1 ack roundtrip in `cli`); 158/158 green.

### Leg 13 — Transports: REST (`tql api`)

Axum server. Adds `axum`, `tokio`, `reqwest`, optionally `utoipa`.

- **Leg 13a** — axum scaffold + core endpoints (DONE 2026-05-11). `src/cmd/api.rs`
  rewritten from stub. Endpoints: `GET /health`, `GET /trackers`,
  `POST /trackers/<name>/add` (body `{input, source}` per DESIGN.md §8).
  `AppState` carries `Arc<Registry>`, `Arc<Engine>`, `Arc<Config>`, and an
  optional API key resolved from `cfg.api.api_key_env`. When the key is
  configured, every endpoint except `/health` requires
  `Authorization: Bearer <key>` or `X-Api-Key: <key>`; when unset the server
  runs open. Handlers reuse `cli::build_torrent_source`, `build_add_params`,
  `build_ack` — same classify→add pipeline, just a JSON-over-HTTP layer.
  10 new handler tests via `tower::ServiceExt::oneshot` (no real socket).
  Adds `axum = "0.7"`; dev-dep `tower = "0.5"`. 168/168 green (+10).

- **Leg 13b** — `GET /trackers/<name>/schema` (DONE 2026-05-11). New
  `src/scripting/schema.rs::to_json_schema(&Manifest) -> serde_json::Value`
  emits a draft-2020-12 JSON Schema for the tracker's input object: per-field
  `type` (string/integer/boolean/array/object), `enum` variants, array
  `minItems`/`maxItems`, `additionalProperties: false` at root,
  `map<string,string>` → `object` with stringly `additionalProperties`,
  descriptions and defaults propagated. Manual translation (no `schemars`
  dep) so we keep `toml::Value` defaults converted in one place. Endpoint
  `GET /trackers/:name/schema` added to `api.rs::router`; auth-gated;
  404 on unknown tracker. 9 new tests (6 in `schema`, 3 in `api`);
  177/177 green.
- **Leg 13c** — `GET /openapi.json` (DONE 2026-05-11). New
  `src/cmd/openapi.rs::build_openapi(&Registry, auth_required) ->
  serde_json::Value` emits OpenAPI 3.1 by hand (no `utoipa` dep). At startup
  every registered tracker contributes concrete paths `/trackers/<name>/add`
  and `/trackers/<name>/schema`, with `components.schemas.<Name>Input`
  reusing `to_json_schema` and a `<Name>AddRequest` wrapper combining
  `input` + `source`. `SourceRequest` modeled as `oneOf` of three tagged
  variants. When `api_key_env` is set the doc declares both `bearerAuth` +
  `apiKeyAuth` security schemes at the document level; `/health` overrides
  with `security: []` to stay public. Endpoint auth-gated like the rest.
  7 new tests (5 in `openapi`, 2 in `api`); 184/184 green. No new deps.

### Leg 14 — Transports: MCP (`tql mcp`)

`rmcp` integration. Split:

- **Leg 14a** — stdio scaffold + JSON-RPC handlers (DONE 2026-05-11).
  `src/cmd/mcp.rs` rewritten from stub. Hand-rolled MCP/JSON-RPC over
  newline-delimited JSON on stdio (no `rmcp` dep yet — same pattern as the
  hand-rolled OpenAPI/JSON-Schema in Leg 13). Methods: `initialize`,
  `notifications/initialized`, `ping`, `tools/list`, `tools/call`. Each
  registered tracker becomes one tool `tracker.<name>.add` whose
  `inputSchema` reuses `to_json_schema` plus a `source` oneOf
  (file/url/magnet). `tools/call` runs the shared classify → qBittorrent
  pipeline; failures surface as `isError: true` tool results (not JSON-RPC
  errors) per spec. Unknown methods → `-32601`; malformed frames →
  `-32700`. Current-thread tokio runtime; `block_on` per stdio frame.
  Protocol version `2024-11-05`. 10 new tests; 194/194 green. No new deps.
- **Leg 14b** — HTTP transport (`--http <addr>`) (DONE 2026-05-11). `mcp.rs`
  grows an axum router (`http_router`) with `POST /` (single JSON-RPC frame
  per request) and `GET /health`. Notifications (no `id`) return `204 No
  Content`; everything else returns `200 OK` with the JSON-RPC envelope —
  even JSON-RPC errors, per spec. Reuses `Server::handle_line` so stdio and
  HTTP go through the same dispatch path. New `Server.api_key` field plus
  `[mcp].api_key_env` config knob: when set, every HTTP request (except
  `/health`) requires `Authorization: Bearer <key>` or `X-Api-Key: <key>`;
  stdio remains unauthenticated (already a trusted local pipe). 6 new
  handler tests via `tower::ServiceExt::oneshot`; 200/200 green. No new
  deps (axum + tower + tokio already in tree). `rmcp` swap + SSE deferred.

### Leg 15 — Notifications & media-server refresh

Telegram, Plex/Jellyfin. Split:

- **Leg 15a** — Notification JSONL spool primitive + post-process wiring (DONE
  2026-05-11). New `src/notify/mod.rs` with `Event` (schema_version, ts,
  info_hash_v1, name, category, link_sites_added, link_sites_removed,
  warnings), `default_spool_path(<library_root>) →
  <library_root>/.metadata/notify.spool`, `enqueue` (O_APPEND + exclusive
  `flock`, parent mkdir, single JSONL line), `read_all` (shared flock,
  NotFound → empty, malformed → InvalidData). `Notify` config grows an
  optional `spool_path` override. `post_process::process_with_cfg` now
  diffs prior vs applied sites and enqueues an event whenever any site
  was added or removed; enqueue failure becomes a warning (never an
  abort — post-process must always exit 0 per §7). No event when the
  re-run produces no diff (idempotent path stays silent). 10 new tests
  (7 in `notify`, 3 in `post_process`); 210/210 green. No new deps.

- **Leg 15b** — `tql notify-flush` CLI + Telegram backend (DONE 2026-05-11).
  New `notify::{drain,commit_drain,requeue,flushing_path}` atomically
  rename the spool to a sibling `.flushing` under exclusive flock; the
  drainer parses, dispatches, and on partial failure appends the unsent
  tail back via `enqueue` (ordering best-effort). `notify::telegram`
  hosts `format_message` (HTML-escaping) + `send_batch(base_url, token,
  chat_id, parse_mode, events)` — the base URL is injectable for tests.
  `cmd/notify_flush.rs` exposes `flush(&cfg, &args, base_url) -> Outcome`
  (`Ok{sent,requeued} | Debounced | DryRun | Error`). `--force` bypasses
  the 5 s mtime debounce, `--dry-run` prints without draining, `--limit
  N` caps events per run. `MAX_BATCH=10` per §15. Backend selection
  honors `cfg.notify.default` and otherwise auto-picks `telegram` when
  `[notify.telegram]` is set; with nothing configured we log to stderr
  and succeed. 15 new tests (4 in `notify`, 5 in `notify::telegram`,
  6 in `cmd::notify_flush`); 225/225 green. No new deps.

- **Leg 15c** — Media-server refresh (DONE 2026-05-11). New
  `src/media/{mod,plex,jellyfin}.rs`. `refresh_all(&cfg, &abs_paths)` (async)
  fans out per configured backend; `refresh_blocking` wraps it in a private
  current-thread tokio runtime for the sync post-process caller. Plex backend
  issues `GET <url>/library/sections/<id>/refresh?path=...&X-Plex-Token=...`
  per (section, path) pair; Jellyfin batches everything into a single
  `POST <url>/Library/Media/Updated` with an `X-Emby-Token` header. 5 s
  per-request timeout (DESIGN.md §15), no retry. Every transport / non-2xx
  / missing-env response folds into the post-process `warnings` vector —
  the library tree and sidecar remain authoritative, refresh is purely
  best-effort. `post_process::process_with_cfg` triggers refresh only for
  *newly added* link sites (idempotent re-runs stay silent, same as the
  notify spool). `media::site_abs_paths` helper builds the
  `<library_root>/<category>/<rel>/<name>` targets the media servers
  actually want to scan. 9 new tests (3 in `media::mod`, 3 in `plex`,
  3 in `jellyfin`); 234/234 green. No new deps (reqwest + tokio + serde_json
  already in tree).

### Leg 16 — `tql doctor` full checks; `tql reload`; polish

Final integration, end-to-end docs.

- **Leg 16a** — `tql doctor` deep checks (DONE 2026-05-11). `cmd/doctor.rs`
  rewritten from a stub-summary into a real checklist with `Status::{Ok,
  Warn, Fail}` per check and a single aggregate exit code (1 on any FAIL).
  Static checks: paths.seed_root + paths.library_root exist as dirs;
  paths.same_fs (`MetadataExt::dev()` equality — FAIL because cross-device
  defeats §9 linking); paths.metadata_dir auto-create + write-probe;
  trackers.root via `load_dir`; trackers.fixtures via `fixtures::run_all`
  (reusing the same engine as `tql test`); qbittorrent login + new
  `Client::app_version` probe (skipped with WARN when unconfigured, FAIL
  when env unset). `--probe` adds telegram `getMe`, plex `/identity`,
  jellyfin `/System/Info/Public` through a shared `http_probe` with a 5 s
  timeout. 4 new tests (238/238 green). No new deps.
- **Leg 16b** — `tql reload` PID-file + signal dispatch (DONE 2026-05-11).
  New `src/pidfile.rs` (run-dir resolution, atomic write, stale-PID-aware
  read, send_sighup) and `scripting::registry::RegistryHandle`
  (`Arc<RwLock<Arc<Registry>>>` with `load`/`swap` so handlers see
  consistent per-request snapshots). `cmd/api.rs` and `cmd/mcp.rs` (HTTP
  mode only) write `<role>.pid` on start, install a tokio
  `SignalKind::hangup()` listener that rebuilds the registry via
  `load_dir` and `swap`s it in atomically, and remove the PID file on
  SIGINT/SIGTERM. Stdio MCP stays PID-file-free (trusted local pipe).
  `cmd/reload.rs` loads config, validates `<trackers_root>` (opt-out via
  `--skip-validate`), and delivers SIGHUP to whichever of `api.pid` /
  `mcp.pid` is live; no live server → warn + exit 0. Adds `libc = "0.2"`
  and the tokio `signal` feature. 8 new tests; 246/246 green.
- **Leg 16c-1** — Bounded `[reconcile] parallelism` (DONE 2026-05-11). `cmd/reconcile.rs`
  rewritten: torrents are first triaged into `Slot::Skip|Run`, then the run set
  is dispatched onto `tokio::task::spawn_blocking` under a
  `tokio::sync::Semaphore` of size `cfg.reconcile.parallelism.max(1)`. Outcomes
  are collected and printed in input order so the summary stays deterministic
  regardless of finish order. Per-hash flock inside `process_with_cfg` keeps
  two concurrent workers from clobbering the same sidecar. `Config` is shared
  across blocking workers via `Arc<Config>`. 1 new test
  (`reconcile_runs_multiple_torrents_with_bounded_parallelism`,
  parallelism=2 over 3 torrents); 247/247 green. No new deps.
- **Leg 16c-2** — Polish: end-to-end docs + Cargo metadata pass (DONE
  2026-05-11). New `README.md` at the repo root: project blurb, build/test
  instructions (NixOS-flavored), subcommand table cross-linked to DESIGN.md
  sections, config search order, tracker layout sketch, license. `Cargo.toml`
  grows `repository`, `readme`, `keywords`, `categories` for crates.io
  hygiene (no publish yet, but metadata is now publish-ready). `cargo check`
  still clean. No code changes; 247/247 still green from Leg 16c-1.

### Leg 17 — Nix flake + devshell (DONE 2026-05-11)

Goal: replace the ad-hoc `nix shell nixpkgs#cargo nixpkgs#rustc nixpkgs#gcc -c`
boilerplate with a real `flake.nix` so contributors can `nix develop --command
cargo …`.

Outcome: `flake.nix` at repo root pinning `nixpkgs-unstable`, exposing a
`devShells.<system>.default` with `cargo`, `rustc`, `rustfmt`, `clippy`, `gcc`,
and `pkg-config` across the four common systems
(x86_64/aarch64 × linux/darwin), plus `formatter = nixpkgs-fmt`. `flake.lock`
committed. `nix develop --command cargo check` succeeds (10.8s). CLAUDE.md's
toolchain section rewritten to lead with `nix develop`, with the legacy
`nix shell` form kept as fallback. No source or Cargo changes.

### Leg 18 — `nix build` package output (DONE 2026-05-11)

Goal: add `packages.<system>.default` to `flake.nix` so `nix build` produces a
ready-to-run `./result/bin/tql` derivation (the leg-17 follow-up that was
explicitly deferred).

Outcome: `flake.nix` grows a `packages` attrset via the existing
`forAllSystems` helper. Each system exposes `default = rustPlatform.buildRustPackage`
pinned to `pname = "tql"`, `version = "0.0.1"`, `src = ./.`,
`cargoLock.lockFile = ./Cargo.lock` (no `cargoHash` churn — Nix vendors from the
lockfile directly). `nativeBuildInputs = [ pkg-config ]`; no `buildInputs`
because reqwest uses rustls. `doCheck = false` — the sandbox can't reach the
ephemeral TCP mocks the test suite spins up; CI keeps running `cargo test`
inside the devshell. `meta` declares dual MIT/Apache-2.0, `mainProgram = "tql"`,
homepage, and platforms. `nix build .#default` succeeds and produces an 11 MB
release binary; `./result/bin/tql --help` lists the full subcommand tree.
README's Build section now leads with `nix build` and keeps `nix develop`
for the iteration loop. No source or Cargo changes.

### Leg 19 — NixOS module (DONE 2026-05-11)

Goal: ship a NixOS module exposing the systemd units described in DESIGN.md
§16 — `tql-api`, `tql-mcp` (HTTP), `tql-reconcile` (timer), `tql-notify-flush`
(timer) — so downstream hosts can `services.tql.enable = true`.

Outcome: `nix/module.nix` with `services.tql.*` options (`enable`, `package`,
`user`/`group`, `configFile` XOR `settings` via `pkgs.formats.toml`,
`environmentFile`, `readWritePaths`, per-unit `api`/`mcp`/`reconcile`/
`notifyFlush` blocks). Hardened systemd defaults
(`ProtectSystem=strict`, `NoNewPrivileges`, `MemoryDenyWriteExecute`,
`Restrict{Namespaces,SUIDSGID,AddressFamilies}`, …). `TQL_CONFIG` points each
unit at the rendered config; `EnvironmentFile` injects secrets. Long-running
units get `Restart=on-failure`; timer units are `Persistent=true`. `flake.nix`
exposes `nixosModules.default` (auto-defaulting `services.tql.package` to
`self.packages.${system}.default`) and a `nixosModules.tql` alias. `nix flake
check --no-build` passes. No Rust code touched — 247/247 still green.

### Leg 20 — NixOS VM test under `checks.<system>` (DONE 2026-05-11)

Goal: end-to-end coverage for the leg-19 NixOS module: boot a VM with
`services.tql.enable = true`, verify `tql-api` comes up and answers
`/health` + `/trackers`.

Outcome: new `nix/test-module.nix` (`pkgs.testers.runNixOSTest`) wiring
the module with `api.enable = true`, `paths.*` under `/var/lib/tql/...`,
and `systemd.tmpfiles.rules` to pre-create the per-path subdirs (the
unit's `StateDirectory=tql` only creates `/var/lib/tql` itself). Test
script waits for `tql-api.service` + port 8080, curls `/health` (expects
`ok`), `/trackers` (expects `[]`), and verifies `tql --help` works from
the system PATH. `flake.nix` grows `checks = forAllSystems (pkgs:
optionalAttrs pkgs.stdenv.isLinux { nixos-module = ...; })` — gated on
Linux because nixosTest needs KVM/QEMU. `nix flake check --no-build`
passes; `nix build .#checks.x86_64-linux.nixos-module` boots the VM,
all asserts pass in ~21 s. No Rust code touched; 247/247 still green.

### Leg 21 — GitHub Actions CI (DONE 2026-05-11)

Goal: continuous verification of fmt, tests, flake evaluation, and the
release build so regressions surface on every push/PR.

Outcome: `.github/workflows/ci.yml` runs on push to `main` and on every PR.
Single `ubuntu-latest` job; concurrency group cancels superseded runs on the
same ref. Steps: checkout → install Nix (`cachix/install-nix-action@v27`,
unstable channel, flakes enabled) → `DeterminateSystems/magic-nix-cache-action`
for store reuse → `nix develop --command cargo fmt --check` → `nix develop
--command cargo test --bin tql` → `nix flake check --no-build` (eval-only;
the VM check from Leg 20 stays opt-in locally to keep CI minutes sane) →
`nix build .#default`. One-time `cargo fmt` pass applied across the tree to
make the fmt gate green from day one. 247/247 tests still pass. No source
behavior changes.

### Leg 22 — `tql sidecar show <hash>` (DONE 2026-05-11)

Goal: implement the still-stub `tql sidecar show <hash>` subcommand from
DESIGN.md §7. Loads config, locates `<library_root>/.metadata/<hash>.json`,
prints the parsed sidecar as pretty JSON to stdout. Missing or malformed
sidecar → stderr message + exit 1.

Outcome: `src/cmd/sidecar_show.rs` rewritten from stub. `Args` gains an
optional `--config` flag for tests/replays (mirrors `tql test`). `run`
loads config and delegates to a private `show(&Config, &str, &mut impl
Write) -> Result<(), u8>` so tests can assert on the printed JSON without
spawning a process. Reuses `sidecar::sidecar_path` + `sidecar::read` (the
existing shared `flock` path), so concurrent post_process / reconcile
writers can't tear the read. 4 new tests (happy path round-trip, missing
sidecar, malformed JSON, origin enum serialization). 251/251 tests green
(+4). No new deps.

### Leg 23 — `tql link {add,remove}` (DONE 2026-05-11)

Goal: retire the last two Leg-1 stubs. Implement `tql link add <hash>
<path>` and `tql link remove <hash> <path>` per DESIGN.md §7 — update
qBittorrent tags then trigger the same single-torrent pipeline as
post-process.

Outcome: `src/qbit/mod.rs` grows `add_tags`/`remove_tags` (POST form
`hashes=<a|b>&tags=<csv>` to `/api/v2/torrents/{add,remove}Tags`). New
shared `src/cmd/link.rs` exposes `Op::{Add,Remove}` + `run(op, hash, path,
config)`: offline tag-string validation → qBittorrent login → addTags /
removeTags → `torrents_info` re-fetch → re-validate with the canonical
category (StartsWithCategory rule) → synthesize `post_process::Args` →
`process_with_cfg`. `cmd/{link_add,link_remove}.rs` become thin clap
wrappers with an optional `--config` flag. 4 new unit tests; 255/255 green
(+4). No new deps.

### Leg 24 — End-to-end test for `tql link add` against a qBittorrent mock (DONE 2026-05-11)

Goal: close one of the open Leg-23 follow-ups — an integration test that
drives `cmd::link::run(Op::Add, ...)` end-to-end against a stubbed
qBittorrent so the login → addTags → torrents_info → post-process
pipeline is exercised together (not just `apply_op`).

Outcome: `src/cmd/link.rs` gains a `#[cfg(test)]` HTTP mock (`spawn_mock`,
`ok_text`, `ok_json`, `write_config`, `TempDir`; same pattern as
`cmd::reconcile::tests` / `cmd::post_process::tests`). New test
`link_add_end_to_end_creates_link_and_sidecar`: writes a seed file, spins
up the mock to route `/api/v2/auth/login` → `Ok.`, `/api/v2/torrents/addTags`
→ HTTP 200, `/api/v2/torrents/info` → JSON reflecting the just-added tag,
writes a config pointing at the mock, calls `run(Op::Add, "deadbeef",
"Cat/Sub", Some(cfg))`, then asserts (a) request order: login → addTags →
info, (b) hardlink target `<lib>/tracker.tld/Cat/Sub/Book` exists with
the same inode as the seed file, (c) sidecar at
`<lib>/.metadata/deadbeef.json` parses with `name = "Book"` and one
`link_sites[]` entry whose `relative_path = "Cat/Sub"`. Env-var name is
PID-suffixed to avoid races with other tests touching qBittorrent
passwords. 1 new test; 256/256 green (+1). No new deps.

### Leg 25 — End-to-end test for `tql link remove` against a qBittorrent mock (DONE 2026-05-11)

Goal: symmetric to Leg 24 — drive `cmd::link::run(Op::Remove, ...)` end-to-end
against a stubbed qBittorrent so the login → removeTags → torrents_info →
post-process pipeline is exercised together, with the on-disk link site and
sidecar mutated as a side effect.

Outcome: new test `link_remove_end_to_end_unlinks_and_updates_sidecar` in
`src/cmd/link.rs` (reuses Leg-24's `spawn_mock`, `ok_text`, `ok_json`,
`write_config`, `TempDir`). Seeds the world with a hardlinked target at
`<lib>/tracker.tld/Cat/Sub/Book` and a sidecar listing that site, then
runs `Op::Remove`. The mock routes `/api/v2/auth/login` → `Ok.`,
`/api/v2/torrents/removeTags` → HTTP 200, `/api/v2/torrents/info` → JSON
with empty `tags` (reflecting the tag just removed). Asserts (a) request
order: login → removeTags → info, (b) the link target is gone and the
now-empty parent dirs (`Cat`, `tracker.tld`-pruning is up to the category
boundary — `Cat` should be pruned), (c) sidecar `link_sites` is empty
after the run. Env-var name PID-suffixed for parallel-test safety.
1 new test; 257/257 green (+1). No new deps.

### Leg 26 — Clippy gate in CI (DONE 2026-05-11)

Goal: add `cargo clippy -- -D warnings` to the CI workflow so style/quality
regressions surface on PRs alongside fmt + test.

Outcome: fixed 5 real lints in source (`paths.rs` x3, `cmd/link.rs`,
removed dead `cmd::mod::unimplemented`), allowed `clippy::result_large_err`
project-wide via a `[lints.clippy]` table in `Cargo.toml` (the
diagnostic-heavy error enums in this crate are intentional). New CI step
`cargo clippy --bin tql --tests -- -D warnings` between fmt and test.
257/257 tests still green. No new deps.

### Leg 27 — `tql sidecar gc` (DONE 2026-05-11)

Goal: implement the orphan-sidecar garbage collector flagged in DESIGN.md §17.
Removes sidecars whose `info_hash_v1` no longer appears in the live qBittorrent
torrent set, and tears down their link sites first.

Outcome: new `src/cmd/sidecar_gc.rs` with `Args { dry_run, config }`. Pipeline:
load config → require `[qbittorrent]` → login → `torrents_info(default)` →
`BTreeSet<String>` of lowercased hashes. Disk pass scans `<library_root>/.metadata/`,
skips dotfiles (the `.lock` siblings), strips `.json`, lowercases hashes for a
case-insensitive comparison. Per-orphan: read sidecar under shared flock →
`linking::unlink_site` per `LinkSite.resolved_path` with
`<library_root>/<category>/` as the stop boundary → on full success, delete the
sidecar JSON and best-effort drop its adjacent `.lock`. On per-site error the
sidecar is *kept* so a re-run can retry (otherwise we'd forget the resolved
paths). `--dry-run` logs `would unlink/remove` lines and leaves the filesystem
untouched; counters still reflect intent. Exit 1 on any per-orphan error or
qBittorrent failure, 0 otherwise. Summary: `scanned, kept, orphans, removed,
sites unlinked, errors` on stderr. Wired into `main.rs` as `SidecarAction::Gc`.
The factored `gc_with_known(&cfg, &known, dry_run)` is the testable core — no
qBittorrent mock needed in tests because the known-hash set is the injection
point. 6 new tests (orphan happy path with parent pruning, known-hash kept,
dry-run, missing `.metadata/` no-op, dotfile/non-json filtering, mixed-case
match); 263/263 green (+6). No new deps.

### Leg 28 — `tql doctor --json` machine-readable output (DONE 2026-05-11)

Goal: a small operational polish on `tql doctor`. The human-readable
checklist is great for interactive use, but monitoring/CI integrations
want a stable structured payload. Add a `--json` flag that emits one JSON
document instead of the table.

Outcome: `cmd/doctor.rs` grows `Args::json: bool`. `finish` splits into
`tally` + `status_message` + `render_json`, and dispatches on the flag.
JSON shape: `{ checks: [{name, status: "ok"|"warn"|"fail", message}],
summary: {total, ok, warn, fail}, exit_code }`. Exit-code policy is
unchanged (1 on any FAIL, 0 otherwise) — the JSON just echoes it for
parsers that don't observe process status. 2 new tests
(`render_json_shape_and_summary`, `render_json_exit_zero_when_no_failures`);
265/265 green (+2). No new deps.

### Leg 29 — Per-tracker credential fetch for URL .torrent sources (DONE 2026-05-11)

Goal: implement DESIGN.md §8's "Rust core fetches with credentials per config"
clause. Until now, when a transport received `source = url`, the URL was passed
verbatim to qBittorrent's `/api/v2/torrents/add`, which means private trackers
that gate `.torrent` downloads behind a cookie or `Authorization` header could
never be added through `tql api` / `tql cli` / `tql mcp`.

Outcome: new `src/fetch.rs` with `fetch_torrent_with_creds(url, &TrackerCreds)
-> Result<Vec<u8>, FetchError>` (reqwest, 30 s timeout, http(s) only,
`Cookie:`/`Authorization:` headers populated from `cookie_env` /
`auth_header_env`). New `cmd::cli::resolve_torrent_source(&Config, tracker_name,
source, kind)` is the shared resolver: when `kind == Url` *and* the tracker has
credentials configured it does the fetch and returns `TorrentSource::File`;
otherwise it falls back to the existing `build_torrent_source` (magnet/URL
passthrough or local file read). The cli/api/mcp dispatchers now call the
resolver instead of `build_torrent_source` directly. File-name fallback for the
upload pulls the last URL path segment so qBittorrent gets a sensible
`.torrent` name. 6 new unit tests in `fetch::tests` (cookie injection, auth
header injection, missing env, non-2xx surfacing status+body, scheme rejection,
`has_creds`); 271/271 green (+6). No new deps (reqwest + tokio already present).
The credential is read from env on every call, so `tql reload` is not needed
to rotate it.

### Leg 30 — `tql sidecar list` (DONE 2026-05-11)

Goal: companion to `tql sidecar show <hash>` — enumerate every sidecar in
`<library_root>/.metadata/` with a brief summary so operators can see at a
glance what `tql` is tracking without iterating one hash at a time.

Outcome: new `src/cmd/sidecar_list.rs` with `Args { json, config }`. Plain
output is one line per sidecar (`<hash>  <category>  <N> sites  <name>`),
sorted by hash. `--json` emits a pretty JSON array of
`{info_hash_v1, category, name, sites_count, size_bytes, is_directory}`.
`<library_root>/.metadata/` missing → empty result (exit 0); other read
errors on the metadata dir → exit 1. Per-entry read failures don't abort
the listing: a stub entry (hash only, empty category/name) lands in the
output so the operator sees what's broken. Dotfiles (the adjacent
`.<hash>.json.lock` files) and any non-`.json` entry are filtered out the
same way `sidecar gc` does it. Wired into `main.rs` as `SidecarAction::List`.
5 new tests (empty metadata dir, sort order, JSON shape, dotfile/non-json
filtering, malformed-sidecar stub fallback); 276/276 green (+5). No new
deps.

### Leg 31 — End-to-end test for `tql sidecar gc` against a qBittorrent mock (DONE 2026-05-11)

Goal: symmetric to Legs 24/25 — exercise `cmd::sidecar_gc::do_run` end-to-end
against a stubbed qBittorrent so the config-load → login → torrents_info →
gc_with_known pipeline is covered (the existing tests inject `known_hashes`
directly, skipping the HTTP path).

Outcome: new test `gc_end_to_end_against_qbittorrent_mock` in
`src/cmd/sidecar_gc.rs::tests`. Seeds `<lib>/.metadata/deadbeef.json` plus a
hardlinked site at `<lib>/tracker.tld/Cat/Sub/Book` via the existing
`seed_sidecar_with_site` helper. Spawns a TcpListener mock that routes
`/api/v2/auth/login` → `Ok.` and `/api/v2/torrents/info` → `[]` (so the
seeded hash is an orphan). Writes a real config TOML pointing
`[qbittorrent].url` at the mock with a PID-suffixed `password_env`, then
calls `do_run(&Args { dry_run: false, config: Some(cfg_path) })`. Asserts
request order (login precedes info), summary `{scanned:1, orphans:1,
removed:1, sites_unlinked:1, errors:0}`, sidecar file gone, link target
gone, and parents pruned up to the category boundary. Inlines the
spawn_mock / ok_text / ok_json helpers from Legs 24/25 (no shared
test-util module yet — left for a future cleanup). 1 new test;
277/277 green (+1). No new deps.

### Leg 32 — Shared `test_http` helpers (DONE 2026-05-11)

Goal: retire the Leg-31 "no shared test-util module yet" follow-up. The
`spawn_mock` / `ok_text` / `ok_json` trio was copy-pasted into the three
qBittorrent-touching command tests (`cmd/link.rs`, `cmd/reconcile.rs`,
`cmd/sidecar_gc.rs`) — same code, three callers.

Outcome: new `src/test_http.rs` (`#![cfg(test)]`) exposes the canonical
`spawn_mock(handler) -> (url, stop, handle)` HTTP/1.1 mock plus
`ok_text(body, extra_headers)` / `ok_json(body)` response builders. Wired
into `main.rs` behind `#[cfg(test)]`. The three cmd test modules replace
their local copies with `use crate::test_http::{ok_json, ok_text,
spawn_mock};` and drop the now-redundant `std::io::{Read, Write}`,
`std::net::TcpListener`, and `std::thread` imports. Other consumers
(`qbit`, `fetch`, `notify::telegram`, `notify_flush`, `media::{plex,
jellyfin}`) keep their bespoke variants for now — their handler
signatures or response shapes diverge enough that a single helper would
need generics to subsume them; left as a future cleanup. 277/277 tests
still green; clippy + fmt clean. No new deps.

### Leg 33 — info_hash in ack for credentialed URL fetches (DONE 2026-05-11)

Goal: close a small gap from Leg 29. When `resolve_torrent_source` fetches a
`.torrent` via per-tracker credentials, the resulting bytes are uploaded to
qBittorrent as a `TorrentSource::File` and passed to `build_ack` as
`torrent_bytes`, but the original `SourceKind` is still `Url`, so the ack
emitted `info_hash: null`. Operators/clients lose a useful field they get
for free on file uploads and magnets.

Outcome: `cli::build_ack` now prefers computing `info_hash` from
`torrent_bytes` whenever they're present, regardless of `SourceKind`. For
magnets we still fall back to `magnet_btih(source)` when no bytes are
available; for `File`/`Url` the bytes are the source of truth (and `Url`
without bytes — i.e. an uncredentialed URL passthrough to qBittorrent —
still yields `null` as before). 1 new test
(`build_ack_url_source_with_bytes_includes_info_hash`); 278/278 green
(+1). No new deps.

### Leg 34 — `tql sidecar verify` (DONE 2026-05-11)

Goal: complementary to `tql sidecar gc`. While `gc` removes sidecars whose
torrent is gone from qBittorrent (orphans), `verify` checks the inverse
direction: for each sidecar still on disk, does every `link_sites[]` entry
still exist and (for single-file torrents) still share an inode with
`content_path`? Operationally this catches bit-rot/manual deletion under
`<library_root>` and re-copies-instead-of-hardlinks that defeat §9 semantics.

Outcome: new `src/cmd/sidecar_verify.rs` with `Args { json, config }`. Iterates
`<library_root>/.metadata/*.json` (sorted by hash, dotfile/non-json filter
identical to `sidecar list`/`gc`). Per sidecar, checks `content_path` exists
and every `link_sites[i].resolved_path` exists; for `is_directory == false`,
also asserts `dev/ino` equality with `content_path` (directory torrents only
get existence — recursive inode checks are deferred). Issue kinds:
`read_error | missing_content | missing_resolved | inode_mismatch`. Plain
output is one line per issue (or `<hash>  ok`) plus a trailing
`scanned=N ok=N with_issues=N issues_total=N` summary; `--json` emits
`{entries:[{info_hash_v1, ok, issues:[…]}], summary:{…}}`. Exit 1 on any
sidecar with at least one issue, 0 otherwise. Wired into `main.rs` as
`SidecarAction::Verify`. 8 new tests (empty meta dir, hardlinked happy path,
missing resolved path, inode mismatch with independent copy, missing content,
JSON shape, malformed sidecar→read_error, directory existence-only).
286/286 green (+8). No new deps.

### Leg 35 — `tql sidecar gc --json` machine-readable output (DONE 2026-05-11)

Goal: small operational polish on `tql sidecar gc`, mirroring Leg 28
(`doctor --json`) and the `--json` flag already on `sidecar list` / `sidecar
verify`. Monitoring/CI integrations want a stable structured payload so they
can ingest GC outcomes without parsing the human summary line.

Outcome: `cmd/sidecar_gc.rs` grows `Args::json: bool`, plus new public types
`Entry`, `EntryStatus { Kept | Orphan | ReadError }`, and `Report { summary,
entries }`. `gc_with_known` is replaced by `gc_with_known_detailed(cfg, known,
dry_run, quiet)` which returns the full `Report`; the previous Summary-only
wrapper would have been dead code, so it was removed and the existing tests
were migrated to `gc_with_known_detailed(..., true).summary`. The `quiet`
flag (true in JSON mode and in tests) suppresses the per-site `would unlink`
/ `eprintln!` chatter; per-orphan errors are still captured into
`entry.errors[]`. JSON shape: `{ dry_run, entries: [{info_hash_v1, status,
removed, sites_unlinked, errors}], summary: {scanned, kept, orphans, removed,
sites_unlinked, errors} }`. On config/qBittorrent load failure in JSON mode
we emit `{ "error": "<msg>" }` to stdout instead of the stderr line.
Exit-code policy unchanged (1 on any error, 0 otherwise). 2 new tests
(`detailed_report_lists_kept_and_orphan_entries`,
`render_json_shape_and_summary`); 288/288 green (+2). No new deps.

### Leg 36 — `tql sidecar repair` (DONE 2026-05-11)

Goal: complementary action to Leg 34's `sidecar verify`. Where verify only
*reports* missing/inode-mismatched link sites, repair *re-applies* them
from the sidecar's `content_path` so operators can recover from a
manually-deleted target, a copy-instead-of-hardlink that defeats §9, or
any other drift verify would flag.

Outcome: new `src/cmd/sidecar_repair.rs` with `Args { dry_run, json, config }`.
Reuses `sidecar_verify::scan` + `Issue` as the issue discovery pass (no
duplication of the inode/existence logic); per-issue dispatch:
`MissingResolved` → `link_to_site(content_path, resolved_path)`,
`InodeMismatch` → `unlink_site` (stop boundary `<library_root>/<category>/`)
then `link_to_site`. `MissingContent` and `ReadError` are recorded as
unrepairable `skip` actions — we have no source of truth to relink from.
`--dry-run` populates the action list with `Outcome::Planned` and touches
nothing; `--json` swaps the human-readable lines for `{dry_run, actions:[{
info_hash_v1, site, target, action, outcome, error?, reason?}], summary:{
scanned, ok, repaired, planned, skipped, failed}}`. Exit 1 on any `failed`
or `skipped` action, 0 otherwise (a clean re-verify after a `--dry-run`
exits 0 because planned actions don't count as failures, but the original
issue is still present — operators must re-run without `--dry-run` to
actually fix). Wired into `main.rs` as `SidecarAction::Repair`. Linking
strategy honors `cfg.linking.prefer` via the same `map_strategy` helper
used by `post_process`. 7 new tests (missing→relink hardlinks, mismatch
→replace, dry-run is no-op, missing-content→skip+exit-1, empty meta-dir,
clean world no-op, JSON shape); 295/295 green (+7). No new deps.

### Leg 37 — Recursive directory inode verification (DONE 2026-05-11)

Goal: close Leg 34's "directory torrents only get existence — recursive inode
checks deferred" caveat. For directory sidecars, `tql sidecar verify` now walks
both `content_path` and each `link_sites[i].resolved_path` and asserts every
regular file under content has a matching `(dev, ino)` under the site.

Outcome: `sidecar_verify::check_sidecar` grows a directory branch that calls
new `dir_tree_drifted(content, site)`, which walks `content_path` via
`collect_tree` (BTreeMap-keyed by relative path; tracks files by `(dev, ino)`
and symlinks by presence) and reports drift if any file is missing under the
site, has a non-file kind there, or its `(dev, ino)` differs. Symlinks are
existence-checked (linking.rs reproduces them as symlinks, not hardlinks, so
inodes legitimately differ). Bonus entries under the site are tolerated —
the invariant is one-directional (every content file must be linked, extras
are fine). Drift collapses to a single site-level `InodeMismatch` — `sidecar
repair`'s existing Replace path (unlink site + relink whole tree) already fixes
it, so finer granularity wouldn't change the remediation. 3 new tests
(directory ok with hardlinked subtree, missing child flagged, child inode
drift flagged); the existing empty-content/empty-site test still passes
trivially. 298/298 green (+3). No new deps.

### Leg 38 — Wire `tracing` + `tracing-subscriber` (DESIGN §6) (DONE 2026-05-11)

Goal: implement the still-missing logging dependency from DESIGN.md §6
("Logging: `tracing` + `tracing-subscriber`. JSONL to file; human to
stderr."). So far the binary uses ad-hoc `eprintln!` calls for operator
output; nothing emits structured events, and there's no way to capture
runs to a JSONL file for later analysis.

Scope:
- New `src/logging.rs` with `init()` that installs a global subscriber:
  - Human formatter to stderr (default level `info`, overridable via
    `TQL_LOG` env using the `EnvFilter` syntax).
  - Optional JSONL file layer at `$TQL_LOG_FILE` (env-controlled, opt-in;
    keeps config surface unchanged for now). Append-only, parent dirs
    auto-created.
  - Idempotent: `try_init` so tests / repeated calls don't panic.
- `main.rs` calls `logging::init()` immediately after `Cli::parse()` so
  every subcommand gets it for free.
- Add `tracing::info!` startup events to `cmd::api::run` and `cmd::mcp::run`
  (bind address, transport) — proves the wiring works without a major
  `eprintln!` rewrite.
- Tests for the env-resolution helpers (`resolve_filter`,
  `resolve_log_file`) without touching the global subscriber.

Deferred:
- Replacing existing `eprintln!` operator-facing output with
  `tracing::info!` — many tests assert on captured stderr and we'd churn
  them for no functional gain. Leave that as a future leg if needed.
- A `[logging]` config block — env-only knobs are enough until a real
  user asks for TOML control.

Out of scope: SSE for MCP, rmcp swap, KVM CI, crates.io publish.

### Leg 39 — `tql notify-flush --json` machine-readable output (DONE 2026-05-11)

Goal: small operational polish on `tql notify-flush`, mirroring Legs 28
(`doctor --json`) and 35 (`sidecar gc --json`). Monitoring/CI integrations
want a stable structured payload so they can ingest flush outcomes without
parsing the human summary line or the existing per-event JSONL dry-run
format.

Outcome: `cmd/notify_flush.rs` grows `Args::json: bool`. New
`render_json(&Outcome) -> String` emits a single pretty JSON document per
outcome variant:
- `Outcome::Ok` → `{outcome:"ok", sent, requeued}`
- `Outcome::Debounced` → `{outcome:"debounced"}`
- `Outcome::DryRun` → `{outcome:"dry_run", pending:[Event,…]}`
- `Outcome::Error` → `{outcome:"error", message}`

`run` routes through `render_json` in JSON mode and preserves the existing
human-readable lines + per-event JSONL dry-run output otherwise. Config-load
and tokio-runtime errors emit the same `{outcome:"error", …}` shape on
stdout in JSON mode (instead of stderr) so a single parser can consume both.
Exit-code policy unchanged (1 on `Error`, 0 otherwise; config/runtime
errors still exit 2). 4 new tests covering each outcome variant's JSON
shape; 308/308 green (+4 from this leg + 6 carried-over additions from
prior session activity). No new deps.

### Leg 40 — `tql test --json` machine-readable output (DONE 2026-05-11)

Goal: small operational polish on `tql test`, mirroring Legs 28
(`doctor --json`), 35 (`sidecar gc --json`), and 39 (`notify-flush --json`).
CI integrations want a structured payload they can ingest without parsing
the human summary line.

Outcome: `cmd/test.rs` grows `Args::json: bool`. New `render_json(...)` emits
a single pretty JSON document: `{trackers_loaded, load_failures:[…],
summary:{total, passed, failed}, failures:[{tracker, fixture, kind, message}],
exit_code}`. `kind` is one of `io|parse|input|classify|mismatch` (mapped
from `FixtureFailureKind`). Config-load, registry-load, and
unknown-tracker-filter errors emit `{error: "<msg>"}` on stdout in JSON
mode (instead of stderr) so a single parser can consume both. Exit-code
policy unchanged (1 on any load failure or fixture failure, 0 otherwise;
config errors still exit 1 — `tql test` has always treated config-load
failures as fatal). 4 new tests (json pass, json fail-on-mismatch,
render_json shape, render_json with load_failures); 312/312 green (+4).
No new deps.

### Leg 41 — `tql reconcile --json` machine-readable output (DONE 2026-05-11)

Goal: small operational polish on `tql reconcile`, mirroring Legs 28
(`doctor --json`), 35 (`sidecar gc --json`), 39 (`notify-flush --json`), and 40
(`tql test --json`). Monitoring/CI integrations want a stable structured
payload they can ingest without parsing the human summary line + per-torrent
chatter on stderr/stdout.

Outcome: `cmd/reconcile.rs` grows `Args::json: bool`. `do_run` now returns
`Report { dry_run, summary, entries }` instead of bare `Summary`; per-torrent
outcomes are collected into `Entry { info_hash_v1, status, adds, removes,
warnings, error? }` with `EntryStatus = ok|planned|aborted|skipped`. The
previous inline `println!`/`eprintln!` chatter inside the per-outcome loop
moves into `Report::print_human` so JSON mode stays silent. `render_json`
emits `{dry_run, entries:[…], summary:{total, ok, planned, aborted,
warnings}}`. Config/qBittorrent failures emit `{error: "<msg>"}` on stdout
in JSON mode (instead of stderr). Exit-code policy unchanged (1 on any
abort or transport error, 0 otherwise). 3 new tests
(`render_json_shape_and_summary`, `render_json_empty_report`,
`reconcile_json_end_to_end_ok_status`); 315/315 green (+3). No new deps.

### Leg 42 — `tql link {add,remove} --json` machine-readable output (DONE 2026-05-11)

Goal: small operational polish on `tql link add` / `tql link remove`, mirroring
the JSON family on `doctor`/`sidecar gc`/`notify-flush`/`test`/`reconcile`.
Operators driving link mutations from scripts want a stable structured payload
they can ingest without parsing the human warn/error lines.

Outcome: `cmd/link.rs` factored: a new `do_run` returns a `Report` enum
(`Ok { warnings } | Error(msg)`), and `link::run` now takes a `json: bool` flag
and dispatches to either `render_json` (pretty JSON to stdout) or the existing
human-readable stderr lines. `cmd::link_add::Args` and `cmd::link_remove::Args`
each grow a `--json` flag. JSON shape: `{op, hash, path, status: "ok"|"error",
warnings: […]?, error?}`. Exit-code policy unchanged (1 on any error, 0 on
success). 2 new tests (`render_json_ok_shape`, `render_json_error_shape`);
317/317 green (+2). No new deps.

### Leg 43 — HTTP request tracing middleware for `tql api` and `tql mcp --http` (DONE 2026-05-11)

Goal: actually use the global tracing subscriber wired in Leg 38. Until now the
two HTTP servers (`tql api` and `tql mcp --http`) only emit a single startup
`tracing::info!`; per-request observability still relied on ad-hoc `eprintln!`.

Outcome: new `src/cmd/http_trace.rs` exposes `trace_request(req, next)`, a
hand-rolled axum middleware that records `method`, `path`, response `status`,
and `elapsed_ms` per request via `tracing::info!(... "http_request")`.
`/health` requests are suppressed so liveness probes don't drown out useful
traffic. Wired into both `api::router` and `mcp::http_router` via
`axum::middleware::from_fn`. No new deps (axum + tracing already in tree).
2 new tests (status passthrough on `/x`, `/health` passthrough); 319/319 green
(+2). fmt + clippy clean.

### Leg 44 — `tql cli --dry-run --json` machine-readable preview (DONE 2026-05-11)

Goal: small operational polish on `tql cli`. The post-add ack is already JSON,
but `--dry-run` emitted a human-readable preview block. Add `--json` so the
dry-run path is pipeline-friendly too.

Outcome: `cmd/cli.rs` grows `Args::json: bool` and threads it through `dispatch`.
New `render_preview_json(tracker, source, kind, input, output) -> String` emits
a single pretty JSON doc `{dry_run: true, tracker, source: {kind, value}, input,
link_tags, info_tags, warnings}`. The flag is a no-op without `--dry-run` (the
post-add ack is unconditionally JSON already). 1 new test
(`render_preview_json_shape`); 320/320 green (+1). No new deps.

### Leg 45 — `--hash` filter on `tql sidecar verify` / `tql sidecar repair` (DONE 2026-05-11)

Goal: small operational polish. Operators triaging a specific torrent
already know its `info_hash_v1`; making them re-verify or re-repair the
entire `.metadata/` tree to act on one sidecar is wasteful and noisy. Add
a `--hash <HASH>` filter to scope the run.

Outcome: `cmd/sidecar_verify.rs::Args` and `cmd/sidecar_repair.rs::Args`
each grow `--hash <HASH>` (case-insensitive). `verify()` and `repair()`
gain a `hash_filter: Option<&str>` parameter; after `scan()` returns the
full sidecar set, the filter prunes via `entries.retain`. If the filter
matches nothing, both commands print `no sidecar matches hash <H>` on
stderr and exit 1 (a clear "you typed the wrong hash" signal rather than
a silent zero-scan). Filter is purely a scope reduction — exit-code
policy, JSON shape, and summary fields are unchanged; the JSON `scanned`
just reflects the filtered count. 4 new tests
(`verify_hash_filter_limits_scan_and_is_case_insensitive`,
`verify_hash_filter_no_match_is_error`,
`repair_hash_filter_only_touches_matching_sidecar`,
`repair_hash_filter_no_match_is_error`); 324/324 green (+4). No new deps.

### Leg 47 — `--category` filter on `tql sidecar verify` / `tql sidecar repair` (DONE 2026-05-11)

Goal: complete the operational-filter trio. Leg 45 added `--hash` to
verify/repair; Leg 46 added `--category` to list. Operators who want to
verify or repair a single tracker's footprint ("all `books.org` sidecars,
nothing else") currently have to pipe through grep on the human output,
which can't drive the per-sidecar repair planning.

Outcome: `cmd/sidecar_verify.rs::Entry` grows `category: Option<String>`
(None on read_error — those don't expose a parseable category). `scan()`
populates it via `verify_one()` from the loaded `Sidecar`. Both
`verify()` and `repair()` take a new `category_filter: Option<&str>`
parameter and `Args` gets `--category <CAT>` (case-insensitive,
`to_lowercase()` on both sides). Same exit-1-on-no-match contract as
`--hash`. 4 new tests
(`verify_category_filter_restricts_scan_case_insensitively`,
`verify_category_filter_no_match_is_error`,
`repair_category_filter_only_touches_matching_sidecar`,
`repair_category_filter_no_match_is_error`); 330/330 green (+4). No new
deps.

### Leg 46 — `--category` filter on `tql sidecar list` (DONE 2026-05-11)

Goal: small operational polish, parallel to Leg 45. When an operator wants
"all sidecars under tracker X", grepping the plain output works but is
fragile for `--json`. Add a first-class filter.

Outcome: `cmd/sidecar_list.rs::Args` grows `--category <CAT>` (case-insensitive).
`list()` gains a `category_filter: Option<&str>` parameter; after `collect()`
returns the full set, `summaries.retain(|s| s.category.to_lowercase() == needle)`
prunes. Unlike Leg 45's verify/repair, an empty result is exit 0 — a list
command asking "any of these?" should answer truthfully rather than treating
zero-matches as a typo. 2 new tests
(`list_category_filter_restricts_results_case_insensitively`,
`list_category_filter_with_no_match_returns_empty`); 326/326 green (+2). No
new deps.

### Leg 48 — `tql reload --json` machine-readable outcome (DONE 2026-05-11)

Goal: round out the JSON family that already covers `doctor`, `sidecar
gc/list/verify/repair`, `notify-flush`, `test`, `reconcile`, `link
add/remove`, and `cli --dry-run`. `tql reload` is the last operational
command still emitting only human lines; operators driving reload from
CI/cron want a structured payload they can ingest.

Outcome: `cmd/reload.rs` factored. `run` now delegates to `do_run(&Args)
-> Outcome` and dispatches to either `render_json` (pretty JSON to
stdout) or the existing human-readable warn/info lines. New `Outcome`
enum has `Ok { validated, load_failures, signaled, errors }` and
`Error(String)` for fatal config/trackers-root failures. `Args` grows
`--json`. JSON shape: `{outcome, validated, load_failures, signaled:
[{role, pid}], errors, no_server}` for the normal path, `{outcome:
"error", message}` for fatal-load. `errors` non-empty flips the outcome
tag to `"error"` and exits 1. Exit-code policy unchanged. 4 new tests
(`render_json_no_server_shape`, `render_json_signaled_shape`,
`render_json_error_outcome_shape`,
`render_json_errors_field_flips_outcome_to_error`); 334/334 green (+4).
No new deps.

### Leg 50 — `--hash` / `--category` filters on `tql sidecar gc` (DONE 2026-05-11)

Goal: bring `tql sidecar gc` to parity with the filtering options already on
`tql sidecar list` (Leg 46) and `tql sidecar verify/repair` (Legs 45/47).
Scoping a gc run to a single torrent or single category is the natural
operator workflow when an incident is contained to one tracker.

Outcome: `cmd/sidecar_gc.rs` `Args` grows `--hash <HASH>` and
`--category <CAT>` (both `Option<String>`, case-insensitive). They're
threaded into `gc_with_known_detailed(cfg, known, dry_run, quiet, hash,
category)`. Hash filter is applied directly to the collected
`(hash, path)` list; category filter peeks at each sidecar with
`sidecar::read` and keeps only those whose `category` matches. A no-match
filter records `summary.errors += 1` and returns, so the existing run-level
`if errors > 0 { Err(1) }` propagates a non-zero exit. The
`fetch_known_hashes` qBittorrent fan-out stays unchanged — we always ask
qBittorrent for the full live set; filtering only narrows the local sidecar
list, so partial-scope gcs cannot accidentally orphan something outside the
filter. 4 new tests (hash-match + no-match, category-match + no-match) plus
a shared `seed_sidecar_with_category` helper. 341/341 green (+4). No new
deps.

### Leg 49 — `tql completions <shell>` shell completion generator (DONE 2026-05-11)

Goal: operational quality-of-life. All operational commands now have JSON
output; the CLI surface itself is the next ergonomic gap. Shell completions
make the per-subcommand `--hash`, `--category`, `--json`, etc. flags
discoverable without leaving the shell, and `clap_complete` derives them
straight off the existing `clap` tree, so this is one small subcommand and
zero risk to the rest of the code.

Outcome: new `cmd/completions.rs` with `Args { shell: Shell }` (clap's
`ValueEnum` covers bash/zsh/fish/elvish/powershell). `run` calls
`clap_complete::generate` against `crate::Cli::command()` and writes to
stdout. `Cli` was made `pub` so the generator can reach it from
`cmd::completions`. Three tests
(`bash_completion_mentions_binary_name_and_subcommand`,
`zsh_completion_is_nonempty`, `fish_completion_is_nonempty`) exercise
`render` (a `#[cfg(test)]` helper that captures the bytes into a `String`).
337/337 green (+3). Adds `clap_complete = "4"`.

### Leg 51 — `tql config show` subcommand (DONE 2026-05-11)

Goal: round out operational visibility. Operators driving `tql` from cron/CI
regularly need to know *which* config file actually got loaded (the search
order is env-dependent) and *what* the effective values look like after the
`$TQL_*` env-overlay pass. Until now the only way to find out was to grep
`src/config.rs` or run `doctor`, neither of which dumps the full effective
tree.

Outcome: new `src/cmd/config_show.rs` with `Args { config: Option<PathBuf>,
path_only: bool }`. Default mode pretty-prints `{path, config}` as JSON,
where `config` is the `serde_json` round-trip of the loaded `Config`.
`--path-only` shortcuts to just the resolved path (useful for
`cat $(tql config show --path-only)` style shell composition). Wired into
`main.rs` under a `Config` subcommand with a `Show` action (leaving room
for a future `Config Validate`). Safety: `Config` stores only env-var
*names* (`*_env`), never secrets, so the JSON dump is always safe to share.
3 new tests (`show_path_only_prints_just_the_path`,
`show_default_emits_pretty_json_with_path_and_config`,
`show_does_not_leak_secret_values_only_env_names`); 344/344 green (+3).
No new deps.

### Leg 52 — `tql config init` starter-config scaffolder (DONE 2026-05-11)

Goal: close the bootstrap gap left by Leg 51. `config show` reports what's
loaded, but a brand-new operator has no file yet — they were copying
sections out of DESIGN.md §11 by hand.

Outcome: new `src/cmd/config_init.rs` + sibling
`config_init_template.toml` (brought in via `include_str!`). Args:
`--output PATH` to override target, `--force` to overwrite an existing
file, `--stdout` to print without touching the filesystem. Default
target: `$XDG_CONFIG_HOME/tql/config.toml` (or `$HOME/.config/tql/…`),
parents created on demand. Wired under `Config::Init`. Template
references secrets via `*_env` names only — guarded by a test that
greps for plaintext field-name patterns. A second test writes the
template through `config::load` to keep the template in lockstep with
the `Config` struct. 5 new tests; 349/349 green (+5). No new deps.

### Leg 53 — `tql config validate` static structural checks (DONE 2026-05-11)

Goal: close the "deeper static-check counterpart to `doctor`" item from
the Leg 52 future-work list. `doctor` mixes static checks with network
probes and fixture execution; operators rolling out a new config to a
machine that can't yet reach qBittorrent (or that has no trackers/ yet)
need a purely offline structural validator.

Outcome: new `src/cmd/config_validate.rs` wired under
`ConfigAction::Validate`. Checks:
- `paths.*` are absolute and resolve to directories,
- URL fields (`qbittorrent`, `media.plex`, `media.jellyfin`) parse and
  use `http`/`https`,
- every `*_env` reference for an enabled section names an env var that
  is actually set (value never printed — guarded by a leak test),
- `[mcp]` with `transport = "http"` requires `api_key_env`,
- every `[trackers.<name>]` credentials block has a matching manifest
  under `paths.trackers_root` (manifest load only — no fixture
  execution, no network).

Output format reuses `cmd::doctor::{Check, Status, render_json}` so
`--json` matches `doctor`'s shape (consumers can reuse the same
parser). 8 new tests; 357/357 green (+8). No new deps.

### Leg 54 — NixOS VM check against a real qbittorrent-nox (DONE 2026-05-11)

Goal: per the project prompt, once the feature surface is complete the
next focus is integration with NixOS/HM modules plus end-to-end coverage
against a real qBittorrent. The existing `nixos-module` VM check only
exercised the bundled `/health` endpoint; nothing in CI ever talked to a
real qBittorrent WebUI. Add a second VM check that boots both
`qbittorrent-nox` and `tql-api`, then asserts `tql doctor --json`
reports the qbittorrent login + version probes as `ok`.

Outcome: new `nix/test-qbittorrent.nix` boots a NixOS VM that runs
`qbittorrent-nox` under a dedicated `qbt` user with a pre-seeded
`qBittorrent.conf` (canonical PBKDF2 hash for the well-known
`admin`/`adminadmin` credential pair, CSRF/HostHeader checks off so
localhost calls don't bounce). `services.tql` is configured with a
matching `[qbittorrent]` block and an `environmentFile` that injects
`TQL_QBIT_PASSWORD=adminadmin`. The test script waits for both units,
proves the WebUI accepts the seeded password directly, then invokes
`tql doctor --config <rendered-path> --json` via `systemd-run
--property=EnvironmentFile=…` (so the password env var is loaded for
the ad-hoc invocation), parses the JSON, and asserts the
`qbittorrent.login` and `qbittorrent.version` checks are `status: ok`.
The conf path gotcha — qBittorrent's `--profile=DIR` reads
`DIR/qBittorrent/config/qBittorrent.conf`, not `DIR/.config/...` —
was caught on the first VM run and corrected. `flake.nix` exposes the
new test as `checks.<system>.nixos-qbittorrent`. `nix build
.#checks.x86_64-linux.nixos-qbittorrent` passes locally
(~22s test-script wall-clock once the system image is built). No Rust
source changes, no new deps.

Future work remains open-ended (publish to crates.io [name `tql@0.0.1`
already exists on the index — needs a decision], swap hand-rolled MCP
for `rmcp`, add SSE, run the NixOS VM checks in CI under KVM, an
even-more-end-to-end test that adds a real torrent and asserts the
sidecar tree, etc.) — start a new leg when picking it up.

### Leg 55 — Home Manager module (DONE 2026-05-11)

Goal: per the Leg 54 future-work list and PROMPT.md's "integration with
NixOS/HM modules" directive — ship a Home Manager variant of the existing
NixOS module so operators running tql under their user account (no root,
no system service manager access) can deploy it declaratively.

Outcome: new `nix/home-module.nix` mirroring `nix/module.nix` but
emitting `systemd.user.{services,timers}` and `home.packages` instead of
the NixOS system equivalents. All four units (api, mcp, reconcile,
notify-flush) are user-scoped. The systemd-hardening block is dropped
(user-mode systemd doesn't run the unit as root, so most knobs don't
apply) — isolation is delegated to the surrounding user session. Same
`settings` / `configFile` / `environmentFile` / `extraArgs` surface as
the NixOS module.

`flake.nix` exposes `homeManagerModules.default` and
`homeManagerModules.tql`. Eval coverage: new pure-eval check
`checks.<system>.home-module` (`nix/test-home-module.nix`) instantiates
the module under a stubbed module system (option stubs for `home.*` and
`systemd.user.*`, so no dependency on home-manager itself) with every
sub-service enabled, then asserts ExecStart contents, oneshot Type,
OnCalendar values, TQL_CONFIG env, and that the package lands in
`home.packages`. `nix build .#checks.x86_64-linux.home-module` passes
(~1s). Two gotchas captured in EXECUTION.md (deepSeq to force checks,
unsafeDiscardStringContext for `lib.hasInfix` over store-path strings).
No Rust source changes; no new deps.

(Each leg may spawn sub-legs as detail emerges. Reorder freely if priorities
shift; record reordering rationale in EXECUTION.md.)

### Leg 56 — `tql cli` end-to-end VM check against live qbittorrent (DONE 2026-05-11)

Goal: per Leg 54's future-work list ("even-more-end-to-end test that adds
a real torrent and asserts the sidecar tree"), exercise the actual user
workflow — `tql cli <tracker> ...` submitting a real `.torrent` to a
live qBittorrent — and prove the classifier-derived tags land. Scoped
to the submission half only; the sidecar/post-process half requires a
torrent that actually completes downloading and is its own leg.

Outcome: new `nix/test-cli.nix` boots qBittorrent + tql in a NixOS VM,
seeds `trackers/example/` into `/var/lib/tql/trackers/example` via a
store-path symlink (`pkgs.runCommand` derivation + `systemd.tmpfiles`
`L+` rule), generates a 16-byte payload, builds a `.torrent` for it
with `pkgs.mktorrent`, and invokes `tql cli --config <…> example
--url=… --categories=Books/Technical --author=Ada /tmp/sample.torrent`
under `systemd-run` (so `TQL_QBIT_PASSWORD` is loaded from the
EnvironmentFile, same trick as `test-qbittorrent.nix`). Asserts:
1) the `tql cli` ack JSON has `ok=true`, `category="example.org"`, and
the two expected `link_tags` (`link:Books/Technical/Ada`,
`link:_authors/Ada`); 2) qBittorrent's `/api/v2/torrents/info` reports
the same torrent under category `example.org` with both link tags
present in the comma-separated `tags` string. `flake.nix` exposes the
new test as `checks.<system>.nixos-cli`. `nix build
.#checks.x86_64-linux.nixos-cli` passes (~24s test-script wall-clock
once the system image is built). No Rust source changes; no new deps.

Future work: sidecar/post-process E2E — needs a torrent payload that
qBittorrent will recheck-to-complete, then a synthetic invocation of
`tql post-process --hash=<h>`, then assertions on the sidecar JSON
under `<seed_root>/...` and on hardlinks under `<library_root>/...`.
**Picked up in Leg 57.**

### Leg 57 — `tql post-process` end-to-end VM check (DONE 2026-05-11)

Goal: close the open future-work item from Leg 56. Walk the full cli →
qbittorrent → post-process → sidecar/library loop in a NixOS VM, so a
regression in the post-process pipeline (link diffing, sidecar write,
hardlink creation) is caught by `nix flake check`.

Outcome: new `nix/test-post-process.nix`. Boots qbt + tql, submits a
real `.torrent` via `tql cli example …` (same setup as `test-cli.nix`),
queries qBittorrent's `/api/v2/torrents/info` for the resulting
`hash`/`name`/`save_path`/`content_path`/`tags`/`category`/`size`,
materializes a synthetic "downloaded" payload at `content_path` (the
torrent's tracker is bogus, so qbt would never produce one itself),
then invokes `tql post-process --hash … --tags … …` under
`systemd-run --uid=tql --gid=tql` with the same EnvironmentFile trick
used elsewhere. Asserts the sidecar JSON at
`/var/lib/tql/library/.metadata/<hash>.json` carries the expected
`info_hash_v1`/`category`/`name`/`is_directory=false`, that the two
expected `link_sites` (`Books/Technical/Ada` and `_authors/Ada`) are
present, that `warnings` is empty, that the two hardlinks exist under
`/var/lib/tql/library/example.org/...`, and that all three (source +
two link sites) share an inode. Re-runs post-process and re-checks
shape + warnings to prove idempotence.

Two gotchas captured in EXECUTION.md:
1. `users.users.qbt` with `createHome = true` produces `/var/lib/qbt`
   at mode 0700, so the `tql` user can't traverse into a download dir
   nested under it. Pinned qBittorrent's
   `Session\DefaultSavePath` to `/var/lib/downloads/` (separate
   tmpfile rule, owned by `qbt`, mode 0755).
2. `fs.protected_hardlinks=1` (kernel default) refuses `link(2)` when
   the caller doesn't own the source. Worked around by chowning the
   synthetic payload to `tql:tql` before invoking post-process —
   noted in the test as a test-only conceit since real deployments
   either run qbt and tql under the same user or unset the sysctl.

`flake.nix` exposes the new test as
`checks.<system>.nixos-post-process`. `nix build
.#checks.x86_64-linux.nixos-post-process` passes (~23s test-script
wall-clock once the system image is built). No Rust source changes;
no new deps.

### Leg 58 — Customizable notification rendering pipeline (SPLIT)

Goal: implement the two-stage `EventFields → FieldManipulation (Rhai)
→ Template (Handlebars) → NotificationText` pipeline specified in
DESIGN.md §15.1–§15.3, replacing the hardcoded `format_message` in
`src/notify/telegram.rs`. Render is overridable globally (config) and
per-tracker (bundle files), with embedded defaults that reproduce
today's Telegram output verbatim so existing operators see no behavior
change until they opt in.

Too large for one session — split into sub-legs 58a–58d below.

### Leg 58a — Notify render pipeline foundation + embedded defaults (DONE 2026-05-12)

Scope:
1. Add `handlebars` to `Cargo.toml`. Reuse the existing Rhai sandbox
   builder (`scripting::sandbox::build_engine`) so render scripts get
   the same op-count bound as classifier scripts.
2. New module `src/notify/render.rs` exposing
   - `enum RenderTarget { Html, MarkdownV2, Plain }`
   - `fn target_from_parse_mode(&str) -> RenderTarget`
   - `fn render_batch(&[Event], RenderTarget) -> Result<String, RenderError>`
   - `fn render_event(&Event, RenderTarget) -> Result<String, RenderError>`
   using the embedded default script + template.
3. Embed `src/notify/defaults/notify.rhai` and
   `src/notify/defaults/notify.hbs` via `include_str!`. The pair
   must produce byte-identical output to the current `format_message`
   for both `HTML` and non-HTML (legacy plain-text) modes.
4. Rewire `notify::telegram::format_message` to call
   `render::render_batch`; on render failure fall back to a minimal
   "<name> [<category>]" line per event so a broken default cannot
   prevent the drainer from sending. (Tracker/global override failure
   modes land in 58b/58c.)
5. Regression tests in `notify::render::tests` diff old hardcoded
   format against new render for a fixed event matrix.

DESIGN clarification (logged in EXECUTION.md): the Rhai `shape`
function takes a third argument `target: string` ("HTML",
"MarkdownV2", or "Plain") in addition to `(fields, escape)`. DESIGN
§15.1 shows the two-arg signature in its illustrative example, but
reproducing today's HTML-vs-plain layout requires the script to
branch on target. The escape function still encodes backend-specific
character escaping; `target` only carries the structural choice.

### Leg 58b — Global render overrides via `[notify]` config (DONE 2026-05-12)

Outcome: `[notify].script_path` and `[notify].template_path` (both
`Option<PathBuf>`) now flow through a new `RenderConfig` struct in
`src/notify/render.rs`. `render_batch_with(events, target, &cfg)` is
the primary entry point; the no-arg `render_batch` / `render_event`
helpers delegate to `RenderConfig::embedded()`. Either side resolves
independently — operators may swap only the script or only the
template, reusing the embedded counterpart.

Failure modes split out cleanly: `RenderError::OverrideMissing { path,
source }` for unreadable files and `RenderError::OverrideInvalid {
path, source }` for rhai compile / handlebars parse errors. The
telegram drainer's `format_message_with(events, parse_mode, &cfg)`
catches both, logs at WARN, and retries against the embedded defaults
before falling back to the minimal one-line summary — a busted
override can never block a notification batch.

`tql config validate` gained `check_notify_overrides`: existence +
parse/compile of each override file (no synthetic-event runtime
execution — that would cross the "static checks only" line). Three
new validator tests + four new render tests; 372/372 green (+15). New
override stanza commented in `config_init_template.toml` so `tql
config init` advertises the feature. No new deps.

Scope:
1. Extend `config::Notify` with optional `script_path` and
   `template_path` (relative to the config file or absolute, same
   resolution as `paths.trackers_root`).
2. Plumb the resolved paths into `render::render_batch` via a
   `RenderConfig` so the renderer reads them once per drain and
   keeps a compiled `AST` + `Handlebars` instance cached.
3. Failure modes: missing file → `RenderError::OverrideMissing`;
   Rhai compile / handlebars parse → `RenderError::OverrideInvalid`.
   In both cases the drainer logs at `WARN` and falls back to the
   embedded defaults.
4. Surface the new fields in `config show`, `config validate`
   (file-must-exist + parse), and `config_init_template.toml`.

### Leg 58c — Per-tracker bundle override (DONE 2026-05-12)

Outcome: `Tracker` now carries optional `notify_script_path` and
`notify_template_path` populated from `<dir>/notify.rhai` and
`<dir>/notify.hbs` when present (no manifest opt-in needed, matching
the `classify.rhai` precedent). `Registry::find_by_category` does an
O(n) lookup by `manifest.canonical_category` — the drainer is the only
caller and batches are small. `RenderConfig::resolved(global, t_script,
t_template)` merges per-side (tracker wins, else global, else
embedded).

The Telegram dispatcher gained `format_message_grouped` (groups events
sharing the same resolved `(script, template)` pair, renders each run,
joins with `\n`) and `send_batch_resolved` (HTTP send using a resolver
closure). Render failure of a tracker override drops one level to
`global`, then `embedded`, then the minimal one-line summary —
`format_message_chain` encodes the three-level fallback so a busted
tracker file never blocks a notification batch.

`notify-flush` now best-effort-loads the registry once per dispatch and
hands the closure to `send_batch_resolved`; if the trackers root is
unreadable the closure just returns global, preserving prior behavior.
Hot-reload is automatic — every drainer invocation re-walks the
registry.

Tests: registry detects/omits notify override files; `find_by_category`
returns matching tracker or `None`; `RenderConfig::resolved` picks
per-side correctly; `format_message_grouped` switches templates
mid-batch; broken tracker template falls back to global rather than
embedded. 378/378 green (+6). No new deps.

### Leg 58d — Illustrative example tracker override + fixture (DONE 2026-05-12)

Outcome: `trackers/example/notify.hbs` ships a template-only override
that decorates the default script's post-shape fields with a 📦 prefix,
inline `<code>` category, and ↑/↓ link-diff glyphs. No `notify.rhai`
sits alongside it, so the per-side fallback wires the embedded default
`shape()` straight into the tracker-supplied template.

Coverage: new `notify::render::tests::example_tracker_template_override_
renders_with_default_script` loads the shipped file via
`CARGO_MANIFEST_DIR` and asserts byte-exact output for a synthetic
`Event` with `category = "example.org"` (the example manifest's
canonical category; DESIGN was paraphrased as "example.tld"). 379/379
green (+1). No new code paths — pure documentation-via-fixture.

### Leg 59 — NixOS check coverage of notify pipeline (PENDING)

With the render pipeline fully integrated (Legs 58a–58d), the next
step per CLAUDE.md is to push the notify path through a NixOS check.
Candidate scope: extend the `test-qbittorrent.nix` VM (or add a sibling
`test-notify.nix`) to point the Telegram drainer at a local HTTP sink
(socat / nc / a tiny python `http.server`) and assert the rendered
payload contains the expected HTML markup for a torrent processed via
the example tracker bundle. To be planned in detail at session start.

### Leg 58 — Original combined description (kept for reference)

Scope:

1. Add `handlebars` to `Cargo.toml`. Reuse the existing Rhai engine
   wiring (same op-count bound from `scripting.max_script_runtime_ms`)
   for the manipulation stage.
2. New module `src/notify/render.rs` exposing
   `fn render(event: &Event, target: RenderTarget) -> Result<String>`
   where `RenderTarget` selects the escape function (Html, MarkdownV2,
   Plain). Internally:
   - Convert `Event` → Rhai object map (`fields`).
   - Resolve script + template by tracker (`event.category`) with the
     three-level fallback from §15.2.
   - Evaluate the Rhai `shape(fields, escape)` function.
   - Render the resolved Handlebars template against the returned map.
3. Embed default `notify.rhai` and `notify.hbs` under
   `src/notify/defaults/` via `include_str!`. The pair must produce
   byte-identical output to the current `format_message` for both
   `HTML` and `MarkdownV2` modes (covered by a regression test that
   diffs old vs. new on a fixed event set).
4. Extend `[notify]` config with optional `script_path` and
   `template_path` for global overrides; surface them in
   `config show` / `config validate` / `config_init_template.toml`.
5. Tracker bundle loader picks up `notify.rhai` / `notify.hbs` if
   present (no manifest entry — presence is the opt-in, matching the
   `classify.rhai` precedent).
6. `src/notify/telegram.rs::format_message` becomes a thin caller of
   `render::render(event, RenderTarget::from(parse_mode))`. Batch
   concatenation logic stays where it is (§15.3 v1 stance).
7. Unit tests: identity-pipeline regression vs. `format_message`,
   per-tracker override wins over global, global wins over embedded,
   escape function varies with target, malformed script/template
   surfaces as `TelegramError::Render` (new variant) rather than
   panicking the drainer.
8. Update `trackers/example/` with an illustrative `notify.hbs` (no
   `notify.rhai` — proves template-only override works) and a fixture
   that asserts its output.

Out of scope (deferred to future legs): batch-level summary templates
(§15.3 future), apprise backend wiring (separate feature-flag leg),
hot-reload of notify scripts on file change (would pair with the
existing `scripting.reload_on_change` plumbing).
