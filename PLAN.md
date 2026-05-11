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

(Each leg may spawn sub-legs as detail emerges. Reorder freely if priorities
shift; record reordering rationale in EXECUTION.md.)
