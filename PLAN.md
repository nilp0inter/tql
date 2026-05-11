# PLAN.md

The implementation plan for `tql`, evolving leg by leg. Each leg is small enough
to finish in one session.

## Status

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

Future work remains open-ended (publish to crates.io, swap hand-rolled
MCP for `rmcp`, add SSE, run the NixOS VM check in CI under KVM,
end-to-end test for `link remove`, etc.) — start a new leg when picking
it up.

(Each leg may spawn sub-legs as detail emerges. Reorder freely if priorities
shift; record reordering rationale in EXECUTION.md.)
