# EXECUTION.md

A chronological log of what was done, why, and what surprised me.

## 2026-05-11 — Session 1

**State at start:** repo had only DESIGN.md, PROMPT.md, .gitignore, and a stray
`nixos.qcow2`. No PLAN.md, EXECUTION.md, CLAUDE.md, no Cargo project.

**Done:**
- Wrote initial CLAUDE.md (toolchain via `nix shell nixpkgs#cargo nixpkgs#rustc`).
- Wrote PLAN.md with 16 legs covering DESIGN.md end-to-end.
- Started Leg 1: scaffolded Cargo project with stub subcommands.

**Decisions:**
- No flake yet. `nix shell` invocations are cheap once the store is warm and
  avoid committing to a devshell shape before we know the dep set.
- Stub commands print "not yet implemented" rather than panicking, so
  `tql --help` and the dispatch tree are exercisable from day one.
- Cargo project sits at the repo root (not in a `tql/` subdir) — DESIGN.md
  shows a `tql/` wrapper but the repo itself is `tql`, so we collapse one
  level.

**Notes for future sessions:**
- `nixos.qcow2` (32 MB) sits in the repo root and is .gitignored already (or
  should be). Verify before committing.

**Outcome:**
- `cargo build` is green (clap deps cached at ~26s clean).
- `tql --help` lists all subcommands; `tql doctor` etc. emit
  "not yet implemented" and exit 0.
- Ran build with `nix shell nixpkgs#cargo nixpkgs#rustc nixpkgs#gcc -c cargo build`
  — `gcc` is required as the linker driver; without it `cargo` fails with
  "linker `cc` not found". Worth recording: future cargo invocations should
  include `nixpkgs#gcc` in the shell.
- Updated CLAUDE.md is missing `nixpkgs#gcc`; will fix in this commit.

## 2026-05-11 — Session 2

**State at start:** Leg 1 landed; only `clap` was a dep. Cargo build green.

**Done:**
- Leg 2: implemented `src/config.rs` with the full DESIGN.md §11 struct tree.
- Added deps: `serde`, `figment` (toml+env features), `toml`.
- Search order: explicit `--config` → `$TQL_CONFIG` → `$XDG_CONFIG_HOME/tql/config.toml`
  (falls back to `~/.config/...`) → `/etc/tql/config.toml`.
- Env override uses `TQL_` prefix with `__` separator (e.g.
  `TQL_PATHS__SEED_ROOT=...` overrides `paths.seed_root`).
- Wired `tql doctor` to parse + print a summary. Non-zero on parse error.
- Smoke-tested with a minimal TOML; missing file → non-zero with figment's
  "missing field" error.

**Decisions:**
- Most sections are `Option<...>` or have `Default`s; only `[paths]` is required.
  Rationale: lets a fresh user run `doctor` against a barebones config and get
  useful feedback. Per-feature legs can tighten requirements as needed.
- `Linking::prefer` modeled as a sum type (`hardlink|reflink|reflink_or_hardlink`)
  using `#[serde(rename_all = "snake_case")]` so impossible values are rejected
  at config-load time rather than at link time.
- Used `figment`'s `Env::split("__")` rather than nested struct flattening, so
  `TQL_QBITTORRENT__URL=...` works without bespoke parsing.
- Did not implement file-exists check for explicit `--config`: figment treats
  a missing TOML as empty, so the surfaced error is "missing field `paths`".
  Acceptable for now; revisit when Leg 16 hardens doctor.

**Outcome:**
- `cargo build` green (1m03s clean with the new deps).
- `tql doctor --config <path>` prints a summary; missing file → exit 1.
- `serde_core` (split from `serde`) appears in the dep graph automatically.

## 2026-05-11 — Session 3

**State at start:** Legs 1+2 landed. Config + dispatch tree are real; everything
else is a stub. No test coverage yet.

**Done:**
- Leg 3: `src/paths.rs` with §10 sanitization and §5 link-tag validation.
- API: `sanitize_component`, `parse_link_tag`, `resolve_link_site`,
  `SanitizeOpts`, `LinkTagError`, `LinkTag`, soft-cap constants.
- 26 tests (unit + 4 property tests via `proptest`). All green.
- Added deps: `unicode-normalization` (NFC), dev-dep `proptest`.

**Decisions:**
- Trailing run of `.`/space collapses to a single `_`, not one-per-char. The
  DESIGN spec is ambiguous ("Replace trailing `.` and trailing space with `_`")
  but the single-`_` reading is what users want for filenames like `foo...`
  (→ `foo_`, not `foo___`). Documented in the test.
- "foo " (trailing whitespace) → "foo", because step 2 (trim) runs *before*
  step 4 (windows-compat). The trailing-space rule only fires when a non-space
  step left a trailing space behind (rare).
- `parse_link_tag` takes an optional `category` so it can serve both producer-
  side (script-host) and consumer-side (post-processor) callers from one fn.
- `resolve_link_site` also re-sanitizes embedded components and rejects them
  if they aren't byte-stable. This catches sloppy script output before any
  filesystem op.
- `EscapesCatRoot` variant is defensive — given lexical pushes and `..`
  rejection, it can't actually fire today. Kept anyway as a safety net for
  future symlink-aware variants.
- Used `proptest` over `quickcheck` because it's the de-facto Rust standard
  and has better shrinking.

**Surprises:**
- `cargo test --lib` errored ("no library targets"). The crate is binary-only;
  tests live inside the bin and are run with `cargo test --bin tql`. Worth
  noting in CLAUDE.md.

**Outcome:**
- `cargo test --bin tql` → 26/26 green in ~0.07s after a 36s clean build.
- Two warnings on unused soft-cap constants (`SOFT_MAX_LINK_TAG_BYTES`,
  `SOFT_MAX_LINK_TAGS`). Will be consumed by Leg 10 (post-process) and Leg 8
  (script-host validator). Left as-is.

## 2026-05-11 — Session 4

**State at start:** Legs 1–3 done. Sanitization + config + dispatch in place.
No sidecar I/O yet; nothing else writes to the filesystem.

**Done:**
- Leg 4: `src/sidecar.rs`. Types `Sidecar`, `LinkSite`, `Origin` matching
  DESIGN.md §14. `read`, `write`, `sidecar_path` helpers.
- Added deps: `serde_json`, `fs2`.
- 8 tests: path layout, missing→None, round-trip, mkdir-on-write,
  overwrite atomicity (no tmp leak), malformed→Parse error, unknown
  schema_version rejected, Origin snake_case serialization.

**Decisions:**
- Lock file is adjacent (`.<hash>.json.lock`) rather than the sidecar itself.
  Reason: writes replace the sidecar via `rename(2)`, which unlinks the old
  inode — holding a lock on the to-be-replaced file gives readers a useless
  guarantee. A separate, stable inode is the only thing that meaningfully
  serializes writers and reader/writer pairs.
- `Origin::CrossSeed` is included now (serde-tagged `cross_seed`) even though
  v1 doesn't emit it. Cheap to add upfront; avoids a schema_version bump if
  we wire cross-seed later.
- No timestamp generation lives in `sidecar.rs`. Callers pass `created_at` /
  `last_applied_at` strings. Keeps the module deterministic and testable
  without a clock abstraction. The `chrono` dep can come with the first
  caller that actually needs it (post_process / reconcile).
- Hand-wrote `SidecarError` rather than pull in `thiserror`. One file, three
  variants — the macro isn't paying for itself yet.
- Pretty-printed JSON for the on-disk format. Sidecars are human-inspected
  (`tql sidecar show`, manual debugging); the size cost is irrelevant at
  one file per torrent.
- `#![allow(dead_code)]` on the module: the public API is consumed by later
  legs (post_process, reconcile, sidecar_show). Without the allow, every
  field would warn until Leg 10 lands.

**Outcome:**
- `cargo test --bin tql` → 34/34 green in 0.06s (24s clean build).
- Still two `paths.rs` soft-cap warnings — unchanged, will land with their
  consumer.

## 2026-05-11 — Session 5

**State at start:** Legs 1–4 done. Config, sanitization, sidecar I/O in place.
No code yet touches the seed/library trees.

**Done:**
- Leg 5: `src/linking.rs`. `link_to_site` + `unlink_site` per DESIGN §9.
- `LinkStrategy` (`Hardlink|Reflink|ReflinkOrHardlink`), `LinkOpts`,
  `LinkOutcome` (`Created|AlreadyCorrect`), structured `LinkError` /
  `UnlinkError`.
- Added dep: `reflink-copy`.
- 9 unit tests: single-file create+idempotent, conflict on unrelated target,
  directory create+inode-check+symlink-preserve, dir-conflict-on-extra-file,
  no temp leak on success, empty-parent pruning up to stop, prune-stops-on-
  sibling, missing-target ok, directory target unlink. 43/43 total green.

**Decisions:**
- Idempotence for directory torrents is *structural + inode-equal* across every
  regular file. Cheaper hashes (size+mtime, count of inodes) leak false-positive
  / false-negative cases for hand-edited library trees. The recursive walk is
  bounded by torrent size, runs on cache-hot inodes, and only fires when `T`
  already exists.
- Temp suffix is `.{name}.tmp.{pid}.{nanos}.{counter}`. Buys uniqueness without
  pulling `rand`/`uuid`. The leading `.` keeps the temp hidden in `ls` output —
  reduces operator confusion if a crash leaves one behind.
- EXDEV is its own error variant (`CrossDevice`), not bucketed into `Io`. The
  caller (post_process) needs to convert this into a fatal config error per §9
  ("EXDEV is a fatal configuration error") — distinct exit code, distinct
  notification copy. Keeping it sum-typed makes that trivial.
- `unlink_site` walks upward via `Path::parent` (lexical), not by `..`. The
  `starts_with(stop)` belt-and-braces check guards against canonicalize racing
  with concurrent mutation. We refuse to ascend above the stop boundary even if
  `read_dir` would say the parent is empty.
- Reflink unsupported-detection: pattern-match raw os errnos (`EOPNOTSUPP=95`,
  `ENOSYS=38`, `EINVAL=22`). `reflink-copy` doesn't expose a typed
  "unsupported" variant on Linux, so we go to errno. `ReflinkOrHardlink` always
  falls back, so this only matters for the `Reflink`-strict path.
- Existing-target check uses `Path::exists()` first, then `symlink_metadata`.
  A dangling symlink at `T` will report `exists()==false`, so we'd then
  try to write under its (missing) parent — but `create_dir_all` is fine with
  that. Edge case; we'll revisit if it ever bites.
- Did not add an integration test that exercises EXDEV. Would need a second
  mounted FS in CI; defer to a manual smoke when we have one. The code path
  is small enough that unit-test coverage of the errno check is adequate.

**Surprises:**
- None significant. `reflink-copy` v0.1 pulled cleanly; no native deps.

## 2026-05-11 — Session 6

**State at start:** Legs 1–5 done. Filesystem layer (sanitization, sidecar,
linking) complete. Nothing yet talks to qBittorrent.

**Done:**
- Split Leg 6 into 6a/6b/6c in PLAN.md. 6a = module skeleton + `login`.
- Added deps: `tokio` (rt-multi-thread + macros + net + time + io-util + sync),
  `reqwest` 0.12 with `default-features=false` and `rustls-tls + cookies +
  json + multipart`.
- `src/qbit/mod.rs`: `Client` with cookie jar + reqwest::Client behind `Arc`,
  `Client::new(base_url)`, `Client::login(user, pass)`. `src/qbit/types.rs`
  placeholder for 6b/6c.
- `Error` enum: `InvalidBaseUrl`, `Http`, `Banned`, `BadCredentials`,
  `UnexpectedBody`.
- 6 tests: login Ok / Fails / 403 / unexpected body / invalid base url /
  base-url trailing-slash normalization. Mock server is a hand-rolled
  `std::net::TcpListener` accept loop on a thread — reads request to
  `\r\n\r\n` + Content-Length bytes, writes a fixed response, closes. No
  extra dev-dep needed.

**Decisions:**
- `default-features=false` on reqwest + `rustls-tls` rather than the default
  `native-tls`. Avoids dragging `openssl-sys` (and its build-time C headers)
  into the dependency tree; rustls is pure Rust and plays nicer with Nix
  builds.
- Login inspects the body (`Ok.` / `Fails.`), not just the HTTP status:
  qBittorrent returns 200 for *both* success and bad-credentials and only
  distinguishes them in the body. HTTP 403 is reserved for the brute-force
  lockout, which gets its own `Banned` variant so callers can surface a
  distinct operator-visible message.
- Set `Referer` to the base URL on login. qBittorrent's WebUI rejects login
  requests whose `Referer` doesn't match its configured host header check.
  Sending one unconditionally is harmless when the check is disabled.
- Hand-rolled TCP mock rather than `wiremock` / `mockito` / spinning up
  `axum`. The mock is ~40 lines and depends on nothing; adding a dev-dep
  for four asserts isn't worth it. `axum` will land properly with Leg 13
  (REST server).
- `Client` is `Clone` via `Arc`-shared jar + reqwest's own internal `Arc`.
  Multi-task callers (reconcile, api) can share one logged-in client
  without re-auth.
- `Client::new` normalizes the base URL to have a trailing slash so
  `url.join("api/v2/...")` always lands at the right place even if the
  user configured `http://host:8080/qbt` (some reverse-proxy setups).

**Outcome:**
- `cargo build` clean in ~1m27s (reqwest + tokio + rustls cold pull).
- `cargo test --bin tql` → 49/49 green in 0.07s.
- Same six `paths.rs` "never used" warnings as before, unchanged.

## 2026-05-11 — Session 7

**State at start:** Leg 6a landed; `Client::login` exercised by mock-server
tests. `add_torrent` / `torrents_info` still missing.

**Done:**
- Leg 6b: `Client::add_torrent(source, &params)` plus public types
  `TorrentSource::{File{filename,bytes}, Url(_)}` and `AddTorrentParams`
  (`category`, `tags: Vec<String>`, `paused`, `auto_tmm`, `savepath`) in
  `qbit/types.rs`.
- New error variants: `AddFailed { status, body }` (HTTP non-2xx, *and* HTTP
  200 with body != `Ok.`) and `NothingToAdd` (empty url string).
- 5 new tests reusing the Leg 6a TCP mock: file upload happy path with
  multipart-body assertions on `torrents`/`category`/`tags`/`paused`/
  `autoTMM`, url upload happy path, `Fails.` body → `AddFailed{200,..}`,
  HTTP 415 → `AddFailed{415,..}`, empty url → `NothingToAdd` without any
  network call. 54/54 green.

**Decisions:**
- `tags` modeled as `Vec<String>` not `String`. The qBittorrent wire format
  is a single comma-separated `tags` field, but the *caller's* shape is a
  list — script-host output (§5), reconcile, post-process all naturally
  carry a list, and the client is the right place to do the join. Future:
  if a tag legally contains `,` we'll need an escape strategy, but §5
  already requires sanitized tags so the join is safe today.
- `Fails.` is bucketed under `AddFailed`, not `BadCredentials`-style. The
  semantic difference matters: login-Fails is binary (bad creds), but
  add-Fails is ambiguous (could be duplicate, malformed torrent, bad
  category, etc.). We surface the raw body so callers can log it; later
  we can add discrimination if we find we need it.
- HTTP-200-non-`Ok.` and HTTP-non-2xx both produce `AddFailed`. Treating
  these uniformly simplifies the caller: any unsuccessful add looks the
  same. `status` is preserved so logs/metrics can split if needed.
- `NothingToAdd` only fires for empty url (not for empty file bytes).
  An empty file is a real (if degenerate) request; qBittorrent will return
  `Fails.` and that's fine. An empty url is a programmer mistake we can
  catch without a round trip.
- File part is sent as `application/x-bittorrent`. qBittorrent doesn't
  actually require this MIME, but setting it is harmless and aids log
  inspection.
- `Referer` header is set on the add call too, same reasoning as login.

**Surprises:**
- The Leg 6a mock reads the request as bytes and only stringifies for
  the handler — but the body assertions in `add_torrent_file_success`
  pass `FAKE_TORRENT_BYTES` (ASCII), so substring matching against
  `String::from_utf8_lossy(...)` works. A future binary-body test (e.g.
  real bencoded `.torrent`) may need a bytes-aware mock, but we don't
  need that yet.

**Outcome:**
- `cargo test --bin tql` → 54/54 green in 0.06s.
- No new deps. `multipart` was already enabled on `reqwest`.

## 2026-05-11 — Session 8

**State at start:** Legs 6a+6b done. qBittorrent client could log in and add
torrents but couldn't list them.

**Done:**
- Leg 6c: `Client::torrents_info(&query)` returning `Vec<TorrentInfo>`.
- `TorrentInfo` in `qbit/types.rs` deserializes the subset of fields tql
  needs: `hash`, `name`, `category` (`""` → `None`), `tags` (CSV → `Vec`),
  `save_path`. Custom serde deserializers handle both normalizations.
- `TorrentsInfoQuery` with optional `hashes` / `category` / `tag` filters.
- 3 tests: list parse + empty-category and CSV-tags normalization, filter
  query serialization (verifies hashes joined by `|` → percent-encoded
  `%7C`), HTTP 403 surfaces as `AddFailed`. 57/57 green.

**Decisions:**
- Reused `Error::AddFailed { status, body }` for non-2xx info responses
  rather than minting a new variant. The semantic shape is identical
  ("qBittorrent said no, here's status + body"); a separate variant would
  multiply call-site matches without buying any precision.
- `hashes` joined with `|`, which is the documented qBittorrent separator.
  `reqwest`'s `.query(...)` builder percent-encodes it to `%7C` on the
  wire — the test asserts that to lock in the format.
- `category` normalizes `""` → `None` at deserialize time. qBittorrent
  reports "no category" as the empty string, but everywhere else in this
  codebase a missing category is `None` (config, sidecar, link tags). The
  conversion belongs at the boundary, not at every consumer.
- `tags` parsed eagerly into `Vec<String>` (trimmed, non-empty). Symmetry
  with `AddTorrentParams::tags`: callers shouldn't care that the wire
  format is CSV.
- Only the five fields we currently need are deserialized. qBittorrent
  returns ~30 (state, progress, eta, ratio, …); `serde_json` silently
  drops the rest. Future legs can grow the struct.

**Surprises:**
- `reqwest::RequestBuilder::query` percent-encodes `|` to `%7C`. Worth
  noting in case a future debug session expects the raw `|` in tcpdump.

**Outcome:**
- `cargo test --bin tql` → 57/57 green in 0.06s.
- No new deps; `reqwest`'s `json` feature was already enabled.

## 2026-05-11 — Session 9

**State at start:** Legs 1–6 done. Filesystem + qBittorrent client in place.
Nothing yet knows what a tracker *is*.

**Done:**
- Leg 7: `src/scripting/{mod,manifest}.rs`. `Manifest` struct, `InputField`,
  `FieldType` (`String|Int|Bool|Array(_)|Enum(Vec<_>)|MapStringString`).
- `parse(&str) -> Result<Manifest, ManifestError>` and `load(&Path)` wrapper.
- 15 unit tests covering happy path (full DESIGN §12 MAM example), parse
  errors, validation failures (charset, duplicate names, unknown type,
  nested array, enum variants, default-type mismatch, min/max_items
  constraint, cli_separator constraint, identifier-shape field names).

**Decisions:**
- Wire types are private (`WireManifest`, `WireInputField`). The public
  API exposes a *validated* `Manifest`; serde deserializes the wire shape
  and a single `validate()` step does all the cross-field/semantic checks.
  Keeps the public surface small and forces validation to run before any
  consumer sees a manifest.
- `FieldType::Enum(Vec<String>)` keeps the variant list ordered (matches
  declaration order) — clap subcommand help (Leg 12) will render them in
  that order, so preserving it matters.
- Manifest types' `default` field is `Option<toml::Value>` rather than a
  fully typed enum. Reasons: (a) defaults need to round-trip through clap
  / JSON Schema verbatim later, (b) typing them now would mean two
  representations to keep in sync. We type-check at parse time so the
  `toml::Value` is guaranteed to match the declared `FieldType` — typed
  retrieval can be a thin helper if Leg 8 wants it.
- `Manifest`/`InputField` derive `PartialEq` but not `Eq`, because
  `toml::Value` doesn't implement `Eq` (it carries `f64`). PartialEq is
  enough for tests and any future "did the manifest change?" check.
- `version` left optional; the §12-mandated warning on missing version
  belongs at the registry/load layer (Leg 9), not at parse.
- `parse_field_type` is recursive so `array<array<string>>` works without
  a special case. `strip_generic` is the only string-fiddly helper, and
  it's small enough that a tokenizer would be overkill.
- No `regex` dep yet for `url_pattern` — we just store it as a string.
  Compilation/validation lands when the transport layer (Leg 12+) actually
  uses it for routing.

**Outcome:**
- `cargo test --bin tql` → 72/72 green in 0.07s.
- No new deps. The §10 soft-cap warnings still ride along; Leg 8 will
  consume them.

## 2026-05-11 — Session 10

**State at start:** Legs 1–7 done. Manifest parser in tree, but nothing
executes Rhai yet.

**Done:**
- Split Leg 8 into 8a (sandbox + classify + validate) and 8b (manifest-typed
  input marshaling).
- Leg 8a: `src/scripting/{sandbox,types,host}.rs`.
- Added dep: `rhai = "1"` with `default-features=false`, `std + sync`. The
  `sync` feature is required because our `Engine` may be wrapped in `Arc`
  and used across threads (transports layer will do this).
- `build_engine(&SandboxLimits)`: `Engine::new_raw()` + the §12-listed basic
  packages (Array/String/Map/Math/Logic). Per-call resource caps wired
  from `SandboxLimits` defaults (200k ops, 64KB strings, 1024-elem arrays,
  32 call depth, 64 expr depth).
- `run_classify(engine, ast, input_map, canonical_category) -> ClassifyOutput`:
  runs `classify(input)`, enforces "must return Map", "link_tags/info_tags/
  warnings are arrays-of-strings", §5 producer rules per tag (re-using
  `paths::parse_link_tag` with the canonical category), soft caps fold
  into warnings, hard caps return `TooLarge`.
- Host helpers registered: `sanitize(s)` (delegates to §10) and `slug(s)`
  (NFKD + `[^A-Za-z0-9._-]` → `_` collapse). The other two helpers from
  §12 (`lower`, `trim`, `replace`) are already standard string methods in
  Rhai once `BasicStringPackage` is loaded — no extra registration needed.
- 12 unit tests added covering happy path, BadReturnShape variants, op
  budget, soft-cap warn folding, §5 violations, and compile errors. Plus
  sandbox-level tests (filesystem access denied, eval disabled, op
  budget, host helpers work). 89/89 total green.

**Decisions:**
- `eval` is a Rhai *keyword*, not just a function. `Engine::new_raw()`
  does **not** disable it — first test pass surfaced this immediately
  (the "no eval recursion" test fired). Fix: `engine.disable_symbol("eval")`.
  Worth a comment in the code so this isn't reintroduced.
- Used `Engine::call_fn` with a fresh `Scope` per run. Scripts are pure
  per §12 ("no I/O, no time, no randomness"), so no scope state is needed
  across calls; reusing a scope would risk cross-tenant leakage if a buggy
  script set a global.
- `ClassifyError::InvalidLinkTag` carries the original `LinkTagError`
  variant so callers can render specific operator-facing copy ("tag X is
  empty" vs "starts with category" vs "contains NUL"). The transport
  layer (Leg 13/14) needs this discrimination for HTTP/JSON-RPC error
  bodies.
- Hard caps (`MAX_LINK_TAGS_HARD=64`, `MAX_INFO_TAGS=64`, `MAX_WARNINGS=32`)
  sit above the §5 soft caps. Soft caps fold into the output's `warnings`;
  hard caps fail the call. Without hard caps a script that bypassed the
  engine's `max_array_size` (e.g., by chained appends inside a function)
  could still produce pathological output. Defense in depth.
- `take_string_array` uses `try_cast` for the array conversion and
  `into_immutable_string` for elements. Rhai 1.24's `ImmutableString` is
  the cheap inspection type; only `to_string()` at the boundary.
- Did not add a `Scripting -> SandboxLimits` converter yet. Config has
  `max_script_runtime_ms` and `max_script_memory_mb`, but the runtime
  cap maps to ops, not ms, and memory is bounded by string/array sizes,
  not bytes. The mapping is a runtime concern (Leg 9 registry) and will
  pick a heuristic there.

**Surprises:**
- Rhai 1.24 ships `try_cast` returning `Option<T>` rather than `Result`.
  Several iterations needed: `result.try_cast::<Map>().ok_or_else(...)`,
  not `.map_err(...)`. Caught at compile time.
- `Engine::new_raw()` *also* leaves `eval` reachable. Fixed via
  `disable_symbol`. The doc on `new_raw` says "no built-in functions,
  type iterators or operators" — but `eval` is a keyword, not a
  function, so it slips through. Saved here for future-me.

**Outcome:**
- `cargo build` clean in ~1m03s after the rhai pull.
- `cargo test --bin tql` → 89/89 green in 0.11s.
- Three new `paths.rs` dead-code warnings (`PATH_MAX`, `resolve_link_site`,
  and the three `LinkTagError` variants that only fire in `resolve_link_site`).
  Will land with their consumer (post-process, Leg 10).

## 2026-05-11 — Session 11

**State at start:** Leg 8a landed; sandbox + classify + producer-rule
validation in place. Scripts could be exercised but only via hand-built
Rhai `Map` inputs.

**Done:**
- Leg 8b: `src/scripting/input.rs` with `marshal_input(&Manifest, &Json)
  -> Result<rhai::Map, InputError>`.
- Validation: top-level must be JSON object; reject unknown top-level
  fields; per-field type check (string/int/bool/array<T>/enum/
  map<string,string>); array `min_items`/`max_items`; enum membership;
  defaults applied from `manifest.toml`; required-without-default fails.
- Optional + no default → field is *omitted* from the output map
  (matches §12 example: `input.author != ()` reads "author absent").
- 12 new unit tests including an end-to-end that pipes a marshaled map
  into `run_classify`. 101/101 green.

**Decisions:**
- Source format is `serde_json::Value`, not `toml::Value`. All three
  transports (REST, MCP, CLI-after-clap) speak JSON natively; the manifest
  is the only thing in TOML. Picking JSON here means transports don't
  re-translate.
- Defaults stay as `toml::Value` in the manifest and are converted at
  marshal time by a parallel `toml_to_dyn` helper. Alternative
  (eagerly converting defaults to JSON at manifest-parse time) would
  bring `serde_json` into the manifest module for no win.
- Unknown top-level fields are rejected. The transport layer is the
  right place to be permissive (e.g., MCP could choose to ignore extras
  for forward compat); the marshaler is strict so CLI typos don't
  silently no-op.
- Optional-and-absent fields are *omitted* from the map rather than
  inserted as `Dynamic::UNIT`. The §12 example script tests
  `input.author != ()` — that idiom works with either encoding, but
  omission is closer to "the schema didn't carry this".
- `BadDefault` is a runtime variant even though manifest validation
  already type-checks defaults. Defense in depth: `type_check_default`
  in the manifest module is shallow (no recursion into nested arrays
  for the value itself); the marshaler walks the structure properly.

**Surprises:**
- None. Rhai's `Dynamic::from_int` / `from_bool` / `Dynamic::from`
  cover all needed conversions; `try_cast::<rhai::Array>()` round-trips
  cleanly in tests.

**Outcome:**
- `cargo test --bin tql` → 101/101 green in 0.10s (12 new tests, no new
  deps).
- Same `paths.rs` dead-code warnings ride along, unchanged.

## 2026-05-11 — Session 12

**State at start:** Legs 1–8 done. Manifest parser + sandbox + classify +
input marshaling all in place. Nothing yet *discovers* the tracker tree;
each test had to hand-build a single tracker.

**Done:**
- Split Leg 9 into 9a (registry) and 9b (fixtures runner + `tql test` CLI).
- Leg 9a: `src/scripting/registry.rs`. `Registry` (BTreeMap-backed,
  stable iteration), `Tracker { manifest, script: Arc<AST>, dir }`,
  `load_dir(root, &Engine) -> Result<LoadReport, RegistryError>`.
- `LoadReport { registry, failures }` separates per-tracker problems
  (collected, non-fatal) from root-level problems (RootMissing /
  RootNotDir / Io, fatal).
- 13 unit tests covering: single + multiple load, ordering, missing
  manifest/script, parse error, compile error, duplicate name, hidden
  dirs skipped, README at root ignored, missing/non-dir root, empty
  root, round-trip into `run_classify`. 114/114 green.

**Decisions:**
- Keyed by manifest `name`, not directory name. Two manifests with the
  same name → second is rejected with `DuplicateName`. The directory
  name is operator-friendly but the manifest's `name` is what callers
  (MCP tool name, REST path, CLI subcommand) actually see; collisions
  there are silent footguns.
- BTreeMap rather than HashMap so `tql cli` (no args) and
  `GET /trackers` render a deterministic list. Per-request lookup is
  O(log n) over O(1), but n ~= dozens; the cost is invisible.
- Per-tracker failures are collected, not fatal. `tql doctor` will
  fail on `!report.ok()`; `tql mcp`/`api` will warn-and-proceed so a
  single broken tracker doesn't wedge the server. Both modes need the
  same data shape — caller decides the policy.
- `Arc<AST>` so `Tracker` can be cheaply cloned into per-request
  tokio tasks once Legs 13/14 land. `Tracker: Clone` is the API
  surface the transports want.
- Hidden dirs (leading `.`) skipped; non-dir entries at root silently
  ignored. Lets users keep `README.md` / `.git` / `.envrc` next to
  their tracker dirs without needing a config knob.
- Inlined a 30-line `tempdir_lite` helper inside the test module
  rather than pull `tempfile` (which has its own deps and is otherwise
  unused in the tree). The helper is good enough: per-pid + nanos +
  atomic counter for uniqueness, RAII cleanup. If a future leg needs
  more (e.g., persistent on failure for debugging), revisit.
- `load_one` returns `MissingManifest` / `MissingScript` *before*
  attempting to read either, so the error is unambiguous (vs a
  bubbled-up `io::Error` ENOENT). Cleaner for `tql doctor` output.

**Surprises:**
- None. The shape of `engine` ownership for compile (`engine.compile`)
  vs run (`engine.call_fn`) made me wonder whether the registry should
  cache its own engine. Decided no: scripts compile *once* against any
  engine with the same package set; runtime uses whichever engine the
  caller has. The transports layer will own one engine per server
  process and pass it to the registry at boot.

**Outcome:**
- `cargo test --bin tql` → 114/114 green in 0.11s.
- No new deps. `paths.rs` warnings unchanged (still waiting on Leg 10).

## 2026-05-11 — Session 13 (Leg 9b)

**Goal:** Fixture runner + `tql test [tracker]` CLI command.

**Done:**
- New module `src/scripting/fixtures.rs` (~310 LOC): `Fixture`,
  `ExpectedOutput`, `FixtureFailure { tracker, fixture, kind }`,
  `FixtureFailureKind { Io | Parse | Input | Classify | Mismatch }`.
- `discover(tracker_dir)` enumerates `fixtures/*.toml` (non-recursive,
  sorted, skips hidden + non-TOML).
- `run_tracker(engine, name, tracker)` and
  `run_all(engine, registry, only)` orchestrate discover →
  `marshal_input` → `run_classify` → equality check.
- `cmd/test.rs`: loads config (same `--config` flag as `doctor`),
  builds the sandboxed engine, loads the registry, runs fixtures,
  prints a summary, returns exit 1 on any failure.
- In-tree example tracker `trackers/example/` (manifest + script + two
  fixtures: with-author and no-author). Drives the end-to-end smoke
  test described below.

**Decisions:**
- Fixture input arrives as TOML but flows through the same JSON
  pipeline as REST/MCP/CLI inputs. Wrote a small `toml_to_json` so
  `marshal_input` (which is `serde_json::Value`-typed) doesn't need a
  parallel TOML variant. Floats fall back to `Json::Null` on
  non-finite — fine for fixtures, which use integers/strings in
  practice. `toml::Value::Datetime` stringifies; we don't expect this
  type in fixtures but better than panicking.
- Per-fixture failures collect into a vector rather than short-
  circuiting. DESIGN.md says "exits non-zero on first failure" but
  *running everything and reporting all failures* is the more useful
  CI behavior — first-failure-only loses signal when a script change
  breaks several fixtures at once. Exit is still non-zero on any
  failure. If we later want strict first-failure, it's a single check
  inside `run_all`.
- `discover` reports parse / IO errors as `FixtureFailure` records
  (not as a hard `Result::Err` propagated up) so one malformed
  fixture file doesn't hide problems in sibling files. Consistent
  with the registry's per-tracker-failure pattern.
- Output comparison is pointwise on `link_tags` / `info_tags` /
  `warnings` in order. Strict order matters because the script
  controls it; if a future tracker emits a multiset, the fixture can
  pre-sort and the script can sort to match. Cheaper to keep
  comparison strict and force scripts to be deterministic.
- `--config` flag on `tql test` (no env-config dependency in tests):
  CI can point it at a per-run config file. The harness fully reuses
  `config::load` + `registry::load_dir`, so any `doctor`-level config
  pathology surfaces here too.
- Unknown tracker filter (`tql test foo` when `foo` isn't registered)
  is an error, not a silent zero-fixture run. Avoids the
  CI-passes-but-tests-didn't-run footgun.
- Inlined the `TempDir` helper again (same as the registry tests) to
  avoid the `tempfile` dev-dep. Two copies is fine; once a third
  module needs it we'll factor it out.

**Smoke test:**
- Pointed `tql test --config <tmp.toml>` at the in-tree
  `trackers/example/`: 2 fixtures, 2 passed.
- First run failed: my example script called `sanitize()` on each
  category, but categories like `"Books/Technical"` are *structural*
  (slash-separated path components) and `sanitize` is a single-
  component function. Fixed the example to trust category strings
  verbatim and only sanitize the free-form `author` field. This is
  the right tracker-author contract per DESIGN.md §10: `sanitize` is
  per-component, not per-path. Worth surfacing in tracker authoring
  docs later.

**Surprises:**
- None major. The TOML → JSON conversion was the only ambiguity; the
  alternative (parse fixture input directly as `serde_json::Value`
  via a `serde` deserializer) would require a custom path because
  TOML's data model has Datetime and JSON doesn't.

**Outcome:**
- `cargo test --bin tql` → 121/121 green (+7 new fixture tests).
- `tql test --config tmp.toml` against `trackers/example/` →
  `1 tracker(s) loaded, 2 fixture(s) total, 2 passed, 0 failed`.
- `tql test --config tmp.toml bogus` → exit 1 + error message.
- No new deps. Cargo build still green.

## 2026-05-11 — Session 14 (Leg 10a)

**State at start:** Legs 1–9 done. Sandbox + fixture runner work; the
qBittorrent hook (`tql post-process`) was still a stub.

**Done:**
- Split Leg 10 into 10a (core pipeline) and 10b (= Leg 15: notifications +
  media refresh). PLAN updated accordingly.
- Rewrote `src/cmd/post_process.rs` (~360 LOC + tests). Stub `Args` gained
  an optional `--config` flag for test/replay; the hook itself never
  receives it from qBittorrent.
- `process(&Args) -> Outcome` is the testable core: load config → flock →
  validate category → parse + resolve `link:` tags → diff vs existing
  sidecar → apply (link new, unlink stale) → write new sidecar. Stale
  removal walks `unlink_site` with `<library_root>/<cat>` as the stop
  boundary.
- `run` wraps it and always returns `Ok(())` per DESIGN §7
  ("Always exits 0").
- Tiny manual ISO 8601 formatter (Hinnant's `civil_from_days`) keeps
  `chrono` out of the tree. 3 small tests pin the format.
- 11 new tests covering: happy path single-file, idempotent re-run, stale
  diff with parent pruning, invalid-tag warns-not-aborts, no-tags writes
  sidecar with warning, empty/slashed category aborts, multi-file
  directory torrent replicates. 132/132 total green.

**Decisions:**
- Used a **distinct lock path** (`.<sidecar>.pp.lock`) for the
  post-process pipeline, separate from `sidecar`'s own
  `.<sidecar>.lock`. Reason: Linux flock keys on open-file-descriptions;
  two opens of the same path from the same process get separate OFDs,
  so an outer pipeline lock on the sidecar's lock would deadlock with
  `sidecar::read`/`write`'s internal acquire. First test run hung
  immediately on this; surfaced in <60 s once cargo tagged the slow
  tests. Worth keeping a comment in the lock helper.
- `Outcome::Aborted(_)` separates "we couldn't even reach the write
  stage" (bad config, lock starvation, malformed sidecar, bad category)
  from `Outcome::Ok` ("wrote a sidecar, possibly with warnings"). Both
  paths exit 0 from `run`; the distinction is for tests + the future
  reconcile/quarantine layer.
- Tag validation aggregates per-tag warnings rather than failing fast.
  A torrent with one bad and one good tag should still produce the
  good link site. The bad tag's reason goes into `sidecar.warnings`
  for operator visibility (DESIGN §5 "Validation is per-tag").
- Apply order is **new sites first, then stale removal**. A retag that
  swaps relative_path keeps the inode reachable continuously; if the
  unlink ran first, a crash mid-pipeline would leave the torrent
  un-multi-homed. Idempotent re-runs hit `LinkOutcome::AlreadyCorrect`.
- `prior_by_rel` preserves the original `created_at` so a sidecar that
  has been around since 2024 doesn't get its timestamps stomped on each
  hook fire. Only newly-introduced sites carry the current run's
  timestamp.
- Skipped quarantine machinery (rename malformed sidecar aside, etc.) —
  for now a malformed sidecar surfaces as `Aborted`. The full
  quarantine flow lands with Leg 11/15 when there's a notification
  spool to file the report into.
- `--config` flag added to `Args` (optional, missing means default
  search). qBittorrent's hook command line never sets it; tests
  always do. Trivial and unblocks all test cases without env-var
  fiddling.
- Hand-rolled ISO 8601 instead of pulling `chrono`. The function is 15
  lines, has three direct tests (epoch zero, Y2K, shape), and avoids
  the largest single dep we'd add at this point.

**Surprises:**
- The flock deadlock. cargo's "test has been running for over 60s"
  diagnostic gave it away cleanly — without that, the test process
  would have just sat there. Reinforces: keep lock files for distinct
  invariants on distinct paths, always.
- Cargo test orchestration via the agent harness is flaky with piped
  background commands; works fine in foreground with output to a
  fixed log file.

**Outcome:**
- `cargo test --bin tql` → 132/132 green in 0.13 s after a ~70 s
  rebuild (no new deps).
- Six warnings on the `paths.rs` soft caps + `PATH_MAX`: now half of
  them are consumed by `post_process`. Remaining warnings will go away
  with Leg 11 (reconcile) which uses the same constants.
- `tql post-process --help` lists every argv flag including the new
  `--config`. qBittorrent's hook command line from §8 still works
  unmodified.

## 2026-05-11 — Leg 11: tql reconcile

**Done.**

- Extracted `process_with_cfg(args, cfg, opts)` from `post_process::process`.
  Old `process` is now a thin wrapper that loads config and forwards with
  `ProcessOpts::default()`. Reconcile holds the loaded `Config` once and
  hands the same reference to each per-torrent pipeline call — no repeated
  TOML parses per torrent.
- Added a third `Outcome::Planned { hash, adds, removes, warnings }` variant
  to carry the dry-run diff back to the reconcile loop. `--dry-run` short-
  circuits *after* tag parsing/validation and `prior_by_rel` construction
  but *before* `link_to_site` / `unlink_site` / `sidecar::write`, so the
  filesystem stays untouched.
- `TorrentInfo` grew `content_path: String` and `size: u64`, both
  `#[serde(default)]` for back-compat with qBittorrent builds (and older
  fixtures) that omit them. Reconcile prefers `content_path`; falls back to
  `<save_path>/<name>` when empty.
- `cmd::reconcile::do_run`: load config → require `[qbittorrent]` →
  resolve password env → tokio current-thread runtime → login →
  `torrents_info` with `--torrent` / `--category` pushed into the query →
  iterate sequentially, building `post_process::Args` per `TorrentInfo` →
  collate a `Summary { total, ok, planned, aborted, warnings }`. Exit 1
  if any torrent aborted or if connection failed; 0 otherwise.
- Torrents lacking a category are skipped with a warning, not aborted —
  TQL is tracker-qualified and a category-less torrent simply doesn't fit
  the layout. (DESIGN §7.3 requires category validity but says nothing
  about how reconcile handles its absence; this seems the least-bad
  default.)
- Sequential iteration only this leg. Bounded parallelism (`[reconcile]
  parallelism`) is a polish step — would need a `tokio::task::spawn_blocking`
  fanout or rayon. Deferred; the per-hash flock that already lives inside
  `process_with_cfg` is what would make it safe.

**Deviations:**
- Per-hash flock concurrency: present in the pipeline core already, so this
  leg gets it "for free." The bounded-parallelism config knob stays
  unwired until a future sub-leg.
- `unused_variable` shadow: `let opts = SanitizeOpts {...}` inside the
  pipeline collided with the new `opts: ProcessOpts` parameter. Renamed
  the SanitizeOpts binding to `sopts` everywhere it was used.
- Five existing post_process tests needed a `Outcome::Planned { .. } =>
  unreachable!(...)` arm added because the enum is non-exhaustive after
  the variant addition. Mechanical edit.

**Outcome:**
- `cargo test --bin tql` → 135/135 green (+3 new in `cmd::reconcile::tests`).
  Roughly 0.1 s after a 71 s rebuild. No new deps.
- The `paths.rs` soft-cap warnings have finally cleared: every constant
  now has a user (post_process + reconcile share the consumers).
- `tql reconcile --help` lists `--dry-run`, `--torrent`, `--category`,
  `--config`. Mock-qbit test exercises the full path including the cookie
  jar round-trip.

## 2026-05-11 — Session N (Leg 12a)

**State at start:** Leg 11 done. `tql cli` was still the original stub
capturing trailing argv. Reading DESIGN.md §7/§13 again to fix the surface
shape (list-with-no-args; manifest-driven flags; positional SOURCE).

**Done:**
- Rewrote `src/cmd/cli.rs`:
  - `Args` now carries `--config <PATH>`, `tracker: Option<String>`, and a
    trailing var-arg `rest: Vec<String>`. Struct uses
    `disable_help_flag = true` so `tql cli <tracker> --help` is forwarded
    into our dynamic subcommand (instead of being consumed by the outer
    clap derive).
  - `dispatch(tracker, rest, &Registry, &Engine)` is the testable core:
    list trackers when `tracker == None`; otherwise build a per-manifest
    `clap::Command` from `build_command`, parse `rest`, materialize JSON
    via `matches_to_json`, then `marshal_input` + `run_classify`, then
    pretty-print a preview block.
  - Source kind detection (`magnet:` / `http(s)://` / file) per §13.
  - qBittorrent add is deferred to Leg 12b; the run ends with an explicit
    "not yet wired" notice and exit 0.

**Decisions:**
- **Leaked strings for clap.** `clap::builder::Str` has `From<&'static str>`
  but *not* `From<String>` (Arc<str> internals). Manifests provide
  `String`s. Building a `clap::Command` dynamically therefore needs `&'static
  str`. Per-invocation leakage (`Box::leak`) is fine because `tql cli` runs
  once and exits; the alternative (TypedValueParser closures + interning) is
  much more code for no real benefit.
- **Required-vs-default at marshal time, not clap time.** clap can't say
  "required unless there's a default in our manifest" without complicating
  the dynamic build. So all clap args are optional; `marshal_input`
  enforces required-with-no-default with its existing, well-formatted
  error. The positional `SOURCE` is the one exception (always required).
- **MapStringString as repeatable KEY=VALUE.** No clap-native map type;
  the simplest CLI shape is `--extra k=v --extra k2=v2`. Bad pairs
  surface a precise error from `matches_to_json`.
- **`cli_separator` → `value_delimiter(first char)`.** The manifest's
  separator is a `String`; clap only knows char delimiters. We take the
  first char and ignore the rest. Manifest validation never restricted
  this to a single char — worth tightening in a later polish leg, but not
  a blocker.
- **`tql cli example --help` works end-to-end.** Setting
  `disable_help_flag = true` on the outer `Args` is what allowed `--help`
  to flow through into `rest` and reach our dynamic command. Verified by
  manual run: shows manifest-derived per-field help text and the SOURCE
  positional.

**Deviations:**
- DESIGN.md §7 says `tql cli <tracker>` should also list the *canonical
  categories* alongside trackers. The current list shows
  `name [canonical_category]` + first description line; "categories"
  plural belongs to a future polish step once we have multi-category
  trackers in the registry — single-category today.
- No tests for the full filesystem `run()` path: it requires a tracker
  dir + a config TOML, which would duplicate the registry tests'
  scaffolding. The `dispatch` function is what's exercised; the manual
  end-to-end run against `trackers/example/` covers the integration.

**Outcome:**
- `cargo test --bin tql` → 145/145 (+10 in `cmd::cli::tests`).
- Manual smoke:
  - `tql cli` → lists `example [example.org]` + description.
  - `tql cli example --url … --categories … --author Alice --labels x
    --labels y /tmp/x.torrent` → marshals input, runs classify, prints
    `link_tags: link:Books/Alice`, `link:Math/Alice`,
    `link:_authors/Alice`, plus `info_tags: label:x label:y`.
  - `tql cli example --help` → manifest-derived help with per-field
    descriptions.
- No new deps. Cargo build ~72 s clean; tests run in ~0.14 s.

## 2026-05-11 — Session 13 (Leg 12b)

**Goal:** wire `tql cli` through to qBittorrent: read or pass through the
source, log in, POST `/torrents/add` with the classified category + tags, and
emit an acknowledgment JSON.

**Done:**
- `src/cmd/cli.rs`: `dispatch` now accepts `&Config` + `dry_run` and, by
  default, builds a `TorrentSource` (file→bytes, magnet/url→passthrough),
  builds `AddTorrentParams { category: canonical_category, tags:
  link_tags ++ info_tags }`, spins up a current-thread tokio runtime, logs
  in, and calls `add_torrent`. On success, prints a pretty-JSON ack
  `{ ok, tracker, category, info_hash, link_tags, info_tags, warnings,
  source: { kind, value } }`.
- `--dry-run` keeps the Leg 12a preview path so script iteration doesn't
  need qBittorrent.
- Helpers (`build_torrent_source`, `build_add_params`, `magnet_btih`,
  `build_ack`) extracted for direct unit-testing.

**Decisions:**
- Acknowledgment `info_hash` is best-effort: extracted from `xt=urn:btih:`
  on magnets, `null` for file/URL sources. Computing it from a .torrent
  would require pulling in a bencode parser; deferring to a polish sub-leg
  rather than blocking transport wiring on it. DESIGN.md §13 step 6
  prescribes the shape but doesn't mandate the hash be populated.
- Tag list is `link_tags ++ info_tags`. qBittorrent stores both as a flat
  CSV; the post-processor cares only about `link:` prefixes, so info_tags
  ride along untouched.
- No end-to-end dispatch test with a mock qBit server: would require
  building a tempdir trackers/ + config + spawn_mock for marginal value
  beyond the per-helper tests and the manual smoke run. The qbit module
  itself already has mock-server coverage for `login` + `add_torrent`.

**Outcome:**
- `cargo test --bin tql` → 151/151 (+6 in `cmd::cli::tests`).
- No new deps.

## 2026-05-11 — Session N (Leg 12c)

**Done:**
- Leg 12c: file-source info_hash now populated.
- New `src/torrent.rs` with a minimal recursive bencode scanner that
  returns only the byte range of the top-level `info` value, never
  re-encodes (BEP 3 requires byte-for-byte SHA1).
- `cli::build_ack` takes `torrent_bytes: Option<&[u8]>`; the dispatcher
  clones bytes out of `TorrentSource::File` before they're consumed by
  `add_torrent`.
- Added `sha1 = "0.10"`.

**Decisions:**
- Hand-rolled bencode scanner instead of pulling in `serde_bencode` /
  `bendy` — we only need the byte range of one key. ~120 lines, no
  abstractions, fully tested.
- Failure to compute (corrupt .torrent, etc.) degrades to `info_hash:
  null` rather than aborting. The torrent already uploaded successfully
  to qBittorrent at that point; failing the ack would be a worse UX than
  a null hash.
- `find_info_range` returns `InfoMissing` if there's no `info` key. In
  practice every .torrent has one, but a typed error beats a panic.

**Outcome:**
- `cargo test --bin tql` → 158/158 (+7).
- Only the `sha1` crate added.

## 2026-05-11 — Session N+1 (Leg 13a)

**Done:**
- Leg 13 split into sub-legs. 13a delivered: axum REST server with three
  endpoints — `GET /health`, `GET /trackers`, `POST /trackers/<name>/add`.
- `src/cmd/api.rs` rewritten from stub. Reuses the existing CLI pipeline
  pieces (`build_torrent_source`, `build_add_params`, `build_ack`,
  `SourceKind`) — same per-tracker classify → fetch → add flow, different
  serialization layer. `cli::build_ack` flipped to `pub(crate)`.
- `AppState` carries `Arc<Registry>`, `Arc<Engine>`, `Arc<Config>`, plus an
  optional `api_key` string resolved at startup from `cfg.api.api_key_env`.
- Auth: when `api_key_env` is set, every endpoint except `/health` requires
  either `Authorization: Bearer <key>` or `X-Api-Key: <key>` matching the
  resolved value. When unset, the server runs open (suitable for
  localhost-only setups). Missing-env-at-startup is fatal.
- `SourceRequest` is a tagged enum (`{kind: file|url|magnet, ...}`)
  matching DESIGN.md §8.
- 10 new tests via `tower::ServiceExt::oneshot` against the axum `Router`
  (no real TCP port): health, list, auth 401/403, bearer accepted, health
  exempt from auth, unknown tracker 404, classify-OK-but-503-without-qbit,
  bad-input 400, source serde round-trip. 168/168 total green (+10).

**Deferred to 13b/13c:**
- `GET /trackers/<name>/schema` — JSON Schema of the input. Needs a
  manifest→schema mapper (subset of what utoipa would generate). Postponed
  along with `/openapi.json`.
- Real end-to-end POST `/add` test that talks to a mock qBittorrent — the
  qbit module already has mock-server coverage for `login` + `add_torrent`;
  duplicating the wiring in axum-land adds setup without coverage value.
- Request logging / `tower-http::trace`. Slot for when we wire `tracing`
  globally.

**Decisions:**
- Reach into the CLI module for shared helpers rather than introducing a
  `transports/pipeline.rs` now. The transports share three primitives;
  abstracting prematurely would pre-commit to a shape MCP (Leg 14) might
  not want. Promote to `transports/` when MCP lands and a third caller
  forces the issue.
- `tower` as a dev-dep only (for `ServiceExt::oneshot`). axum 0.7 already
  pulls in `tower` itself; the dev-dep is solely for the trait import.
- axum 0.7 (not 0.8) — `matchit 0.7` matches the path syntax I already
  knew (`:name`), and 0.8 changes the param syntax. Stay on 0.7 until
  Leg 13b/c forces a bump.
- 503 (not 500) when `[qbittorrent]` is unconfigured: classification
  succeeded, the server is just not set up to fan out. 502 when the
  upstream qBit call itself fails.

**Outcome:**
- `cargo test --bin tql` → 168/168 green.
- New deps: `axum = "0.7"`, `tower = "0.5"` (dev-only). axum brought in
  hyper/hyper-util/tower-http transitively; first build ~100 s.

## 2026-05-11 — Session 17 (Leg 13b)

**State at start:** Leg 13a landed REST scaffold; 168/168 green. Two API
sub-legs deferred: `/trackers/:name/schema` and `/openapi.json`.

**Done:**
- New module `src/scripting/schema.rs` with `to_json_schema(&Manifest) ->
  serde_json::Value`. Hand-rolled translator (no `schemars` dep) — the
  manifest type set is small (six `FieldType` variants), and going manual
  keeps the `toml::Value` default conversion local instead of needing a
  serde bridge.
- Emits draft-2020-12 schema (`$schema` dialect URL set). Root is
  `{type: "object", properties, required, additionalProperties: false}`.
  Per-field: `description`, `default` propagated; arrays carry `minItems`/
  `maxItems` and recursive `items`; enums map to `{type: "string", enum:
  [...]}`; `map<string,string>` → `{type: "object",
  additionalProperties: {type: "string"}}`.
- Wired `GET /trackers/:name/schema` in `api.rs::router`. Reuses
  `check_auth` (so the schema is gated like everything else when an API
  key is configured) and returns 404 for unknown trackers, mirroring
  `/add`'s behavior.

**Decisions:**
- `additionalProperties: false` at the root: matches `marshal_input`'s
  behavior (it rejects unknown top-level keys), so the schema is honest
  about what the server will accept.
- Required omitted when empty (rather than `"required": []`): JSON Schema
  permits either, but an empty array is noise.
- Inner-type-only descent for array items (`type_schema`): per-field
  metadata like `min_items` only applies to the *outer* array; nested
  arrays don't carry their own constraints in the manifest.
- Did NOT bundle the `source` field into the schema. DESIGN.md §7 line
  434 says "JSON Schema of the tracker's input", and the wire body for
  POST `/add` is `{input, source}` — `source` is a transport-level field,
  not a manifest input. If clients want a body-schema later we'll add
  `/trackers/:name/openapi.json` style coverage in 13c.

**Tests added (9):**
- `schema.rs`: primitives, array min/max + inner type, enum + default,
  `map<string,string>`, nested array recursion, required-omitted-when-
  empty.
- `api.rs`: schema endpoint returns the JSON object; unknown tracker 404;
  auth required when configured.

**Outcome:**
- `cargo test --bin tql` → 177/177 green. No new deps.

## 2026-05-11 — Session 19 (Leg 13c)

**State at start:** Leg 13a + 13b landed `/health`, `/trackers`,
`/trackers/:name/schema`, `/trackers/:name/add`. 177/177 green. `/openapi.json`
was the last REST sub-leg before MCP.

**Done:**
- New module `src/cmd/openapi.rs`. `build_openapi(&Registry, auth_required) ->
  serde_json::Value` emits an OpenAPI 3.1 document.
- Per-tracker paths generated at startup: every registered tracker contributes
  both `/trackers/<name>/add` and `/trackers/<name>/schema` so clients see
  concrete request bodies (not just `{name} path param + generic object`).
- Per-tracker input schemas reused via `scripting::schema::to_json_schema`,
  embedded as `components.schemas.<Name>Input`. Wrapper `<Name>AddRequest`
  combines `input` + `source` with `additionalProperties: false`.
- `SourceRequest` modeled as `oneOf` of three tagged variants
  (`file/url/magnet`) so the schema mirrors the runtime serde shape.
- Auth: when `[api].api_key_env` is configured, the doc declares both
  `bearerAuth` (HTTP Bearer) and `apiKeyAuth` (X-Api-Key) security schemes
  at the document level. `/health` overrides with `security: []` to keep
  liveness probes credential-free, matching the runtime exemption.
- `/openapi.json` endpoint wired into `api.rs::router`; still auth-gated when
  a key is configured (no point handing the doc to anonymous callers if
  every other route is closed).

**Decisions:**
- Manual translation (no `utoipa` dep). The surface is small enough that the
  hand-rolled doc is cheaper than wiring proc-macros across a dynamic
  registry. `to_json_schema` is already manual, so this keeps schema logic
  in one place.
- Per-tracker paths instead of a single `{name}` path parameter. Clients can
  generate one client method per tracker, the OpenAPI viewer renders the
  exact input shape, and 404 semantics are encoded by the path's existence.
- Stripped `$schema` and `title` keys when embedding a tracker's input schema
  as a component — they're document-level keys that don't belong inside
  another doc's `components.schemas`. Kept `description`.
- `pascal_case` for component names: alphanumeric run accumulation,
  non-alphanumeric splits words. Fallback `"Tracker"` keeps the doc valid
  even if a manifest name is somehow empty (shouldn't happen — Leg 7
  validates names).

**Tests added (7):**
- `openapi.rs`: pascal_case helper, empty-registry doc shape,
  registry-adds-per-tracker-paths-and-schemas, auth-required-emits-security
  (incl. `/health` exemption), SourceRequest oneOf variants.
- `api.rs`: `/openapi.json` returns 200 with the doc; auth required when
  configured.

**Outcome:**
- `cargo test --bin tql` → 184/184 green. No new deps.
- Leg 13 closes; next is Leg 14 (MCP via `rmcp`).

## 2026-05-11 — Session N (Leg 14a)

**Goal:** stand up `tql mcp` over stdio with JSON-RPC 2.0, exposing each
registered tracker as one `tracker.<name>.add` MCP tool, wired to the same
classify → qBittorrent pipeline as `cli`/`api`.

**Done:**
- Rewrote `src/cmd/mcp.rs` from stub to a real stdio server.
- Implemented `initialize`, `notifications/initialized`, `ping`, `tools/list`,
  `tools/call`; unknown methods get JSON-RPC error `-32601`; malformed
  frames get `-32700`.
- Tool `inputSchema` = `{ input: <to_json_schema(manifest)>, source: <oneOf
  file/url/magnet> }`, matching the REST AddRequest shape.
- Tool failures (input validation, classify, missing qbittorrent, qbit
  transport) surface as MCP tool results with `isError: true`, not JSON-RPC
  errors — that's what the MCP spec specifies for tool-level failures.
- 10 new tests; 194/194 green.

**Decisions:**
- Hand-rolled MCP/JSON-RPC over NDJSON, no `rmcp` dep yet — consistent with
  our pattern of deferring heavy deps (no `utoipa`, no `schemars`). Protocol
  version `2024-11-05` (stable). `rmcp` can replace this in Leg 14b/c if the
  surface grows (resources, prompts, HTTP transport with SSE).
- Stdio reader is `std::io::stdin().lock().lines()` (sync, line-delimited).
  `tools/call` is async because the qBittorrent submission is — we use
  `rt.block_on(server.handle_line(...))` on a current-thread tokio runtime
  per frame. Simple, no concurrency between calls on one connection.
- `--http` flag rejected with a clear "not yet implemented" message rather
  than silently falling back to stdio. Saves a confused user.

**Outcome:**
- `cargo build` + `cargo test --bin tql` green (194/194, +10).
- Leg 14a complete. Leg 14b open for HTTP transport + (optionally) swap to
  `rmcp` once we want resources/prompts/SSE.

## 2026-05-11 — Leg 14b: MCP HTTP transport

**Goal:** wire `tql mcp --http <addr>` to a real axum server so multiple
clients can share one MCP server. Reuse the JSON-RPC dispatcher from 14a.

**Done:**
- `Server` grew an `api_key: Option<String>` field (auth applies to HTTP
  only; stdio remains an unauthenticated trusted local pipe).
- New `[mcp].api_key_env` config knob, resolved at startup the same way
  `[api].api_key_env` is: missing/empty env → fail fast.
- `http_router(server) -> Router` exposes `GET /health` (always open) and
  `POST /` (single JSON-RPC frame per request body). Notifications (no
  `id`) yield `204 No Content`; everything else is `200 OK` carrying the
  JSON-RPC envelope — including JSON-RPC errors. HTTP status is reserved
  for transport/auth concerns; protocol errors live inside the body. This
  matches the JSON-RPC-over-HTTP convention and lets clients use one
  parser path for success and protocol errors.
- 6 new handler tests via `tower::ServiceExt::oneshot`: `/health` open
  even when keys configured; initialize roundtrip; notification → 204;
  invalid UTF-8/JSON → 200 with `-32700`; auth UNAUTHORIZED/FORBIDDEN/OK
  matrix; `X-Api-Key` header alternative accepted.

**Decisions:**
- Body parsing: take `axum::body::Bytes`, hand the raw string to
  `Server::handle_line`. The dispatcher already owns `-32700` semantics,
  so we don't want axum's `Json<Value>` extractor (which would 400
  invalid JSON before we can emit the JSON-RPC envelope).
- Single endpoint `POST /`. The MCP "Streamable HTTP" spec wants `/mcp`
  with SSE for server-initiated messages; we don't have server-initiated
  messages yet (no notifications, no resources/prompts). One-shot JSON-RPC
  over HTTP is sufficient and keeps `rmcp` out of the dep tree for now.
  Swap to `rmcp` when we need SSE.
- Stdio path stays sync (`stdin().lock().lines()`); HTTP path uses a
  multi-thread tokio runtime so requests can run concurrently. The two
  share the same `Server` value (`Clone` via `Arc`s).

**Outcome:**
- `cargo build` + `cargo test --bin tql` green (200/200, +6).
- Leg 14 done end to end except the optional `rmcp` swap, which we'll do
  only if a future leg needs SSE / resources / prompts.

---

## Leg 15a — Notification JSONL spool primitive (DONE 2026-05-11)

**Goal:** Land the file-backed spool DESIGN.md §15 calls for and wire
`post_process` to enqueue an event whenever sites are added or removed.
Backends (Telegram), debounce/batch policy, and the `notify-flush` CLI
are explicitly deferred so this leg stays one session.

**Changes:**
- New module `src/notify/mod.rs` (`mod notify;` in `main.rs`). Public
  surface: `Event` struct (serde, `schema_version`, `ts`, `info_hash_v1`,
  `name`, `category`, `link_sites_added`, `link_sites_removed`,
  `warnings`), `EVENT_SCHEMA_VERSION = 1`, `default_spool_path` =
  `<library_root>/.metadata/notify.spool`, `enqueue(spool, event)` and
  `read_all(spool)`.
- `enqueue` opens `O_APPEND | O_CREATE`, takes an exclusive `flock`,
  writes one JSONL line (`serde_json::to_vec` + `\n`), flushes, drops
  the lock. Parent directories created on demand so the first post-
  process call in a fresh `library_root` doesn't fail.
- `read_all` uses a shared flock, returns `Ok(vec![])` on `NotFound`,
  and surfaces a malformed line as `io::ErrorKind::InvalidData` so the
  drainer (future Leg 15b) can decide whether to quarantine.
- `config::Notify` grows an optional `spool_path: Option<PathBuf>` so
  operators can pin the spool somewhere outside `library_root` (e.g.
  tmpfs for ephemeral notifications).
- `post_process::process_with_cfg`: after the sidecar write, diff
  `applied_sites` against `prior_by_rel` to get the *added* relpaths
  and `prior_by_rel ∖ applied_relpaths` to get the *removed* relpaths;
  if either is non-empty, build a `notify::Event` and `enqueue` it.
  Enqueue failures degrade to a warning — `post-process` must always
  exit 0 (§7), and the sidecar is already on disk as the source of
  truth.

**Decisions:**
- *No event on idempotent re-runs.* If neither set is non-empty, skip
  the enqueue entirely. A torrent that completes, gets re-tagged
  identically, and re-completes shouldn't spam the Telegram channel.
- *No top-level `last_event_id` field in the sidecar.* DESIGN.md §14
  doesn't require it, and we can compute "have I notified about this
  diff?" later from spool semantics. Avoids a schema bump for a feature
  not yet needed.
- *Spool path under `.metadata/`.* Same directory as sidecars so a
  single backup target covers both. `notify.spool` (singular, no hash
  suffix) — the spool is global, not per-torrent.
- *`O_APPEND` + flock rather than a per-torrent file.* Simpler drainer,
  preserves natural insertion order, and `flock` is enough to prevent
  torn lines on Linux (the only target platform that matters for the
  hook). A drainer hitting an in-flight writer just waits for the
  shared lock.
- *Borrow checker nit*: post_process's `now` string is now consumed
  twice (sidecar's `last_applied_at` and the event's `ts`). Cloned
  once at the sidecar build site rather than pre-cloning at the top —
  keeps the timestamp single-sourced.

**Tests added (10, total 210/210):**
- `notify::tests`: default path layout, roundtrip, missing-file →
  empty, parent mkdir on demand, malformed line surfaces error,
  one-line-per-event, 4-thread concurrent enqueue produces exactly
  N×M events.
- `post_process::tests`: `notify_spool_gets_event_with_adds`,
  `notify_spool_records_stale_removal`, `notify_spool_silent_when_no_changes`.

**Outcome:**
- `cargo build` + `cargo test --bin tql` green (210/210, +10).
- No new deps (`fs2` + `serde_json` already in tree).

## 2026-05-11 — Session: Leg 15b (notify-flush + Telegram)

**Goal:** drain the JSONL spool, batch into ≤10-event Telegram messages,
debounce a recent spool, requeue on partial failure. Wire as `tql
notify-flush`.

**Done:**
- `notify::drain` / `commit_drain` / `requeue` / `flushing_path` —
  atomic move-aside under exclusive flock, then plain `read_all` on the
  sibling `.flushing` file. Recovers a leftover flushing file from a
  prior crash (its events lead, fresh spool follows).
- `notify::telegram::{format_message, send_batch}` — HTML escaping, JSON
  POST to `<base>/bot<token>/sendMessage`, surfaces `ok: false` bodies
  as `Api { status, body }`. `MAX_BATCH = 10` per DESIGN §15.
- `cmd/notify_flush.rs` — `flush(&cfg, &args, base_url)` returns an
  `Outcome` enum (`Ok { sent, requeued } | Debounced | DryRun | Error`)
  so tests don't touch process exit. Debounce checks the spool's mtime
  vs 5 s; `--force` bypasses; `--dry-run` reads without draining;
  `--limit` caps events per run.

**Decisions:**
- *Backend selection.* `cfg.notify.default` is honored if non-empty;
  otherwise we auto-pick `telegram` when `[notify.telegram]` is set,
  and fall back to "log to stderr" with success when nothing is
  configured. That keeps a fresh install from blowing up on the first
  enqueue + flush cycle.
- *Telegram base URL is injected.* `send_batch` takes the base so tests
  can point at a `TcpListener` mock. `cmd::notify_flush::run` passes
  `telegram::DEFAULT_BASE_URL` in production.
- *Best-effort ordering on requeue.* Producers writing during a flush
  land in a fresh spool; on partial failure we append the unsent tail
  back, so newer events sit before the retried tail. Acceptable for
  notifications and avoids a second flock dance.
- *No `rmcp`-style framework.* Hand-rolled HTTP via `reqwest::Client`,
  consistent with the rest of the codebase.

**Tests added (15, total 225/225):**
- `notify::tests`: drain moves events + clears spool, drain on missing
  spool is empty, drain recovers a prior `.flushing` file, requeue
  rewrites tail.
- `notify::telegram::tests`: HTML escape, batch newline join, mock
  POST asserts payload, surfaces `ok: false`, surfaces HTTP 500.
- `cmd::notify_flush::tests`: empty spool is a noop, debounce skips
  recent mtime, dry-run prints without draining, success path drains +
  sends + clears, failure requeues, 23 events → exactly 3 batches.

**Outcome:**
- `cargo build` + `cargo test --bin tql` green (225/225, +15).
- No new deps. Uses `std::fs::FileTimes` (stable since 1.75) in tests to
  back-date the spool for the debounce check.
- Leg 15 split into a/b/c; 15a complete, 15b and 15c queued.

## 2026-05-11 — Session N (Leg 15c)

**Goal:** wire Plex + Jellyfin partial refresh from the post-processor.

**Done:**
- New `src/media/{mod,plex,jellyfin}.rs` module tree.
- Plex backend fans out `GET /library/sections/<id>/refresh?path=…&X-Plex-Token=…`
  per (section_id × path). Section IDs come from config; Plex ignores paths
  outside its section, so the cross product is the simplest correct strategy.
- Jellyfin backend batches every path into a single
  `POST /Library/Media/Updated` with `X-Emby-Token`. One request per call.
- Top-level async `refresh_all(cfg, abs_paths)` resolves the
  `*_env` secrets at call time, fans out to whatever is configured. Sync
  wrapper `refresh_blocking` spins a private current-thread tokio runtime
  for the post-process pipeline.
- `post_process::process_with_cfg` invokes `refresh_blocking` only when
  there are new sites (idempotent re-runs stay silent, mirroring the notify
  spool diff logic). Failures fold into `warnings`; never abort.

**Decisions:**
- *5 s timeout enforced at the reqwest client.* No retry — per DESIGN.md §15.
  We surface the failure as a warning and continue. The next reconcile run is
  the safety net (it doesn't re-trigger media refresh today, but a future
  leg can add it; refresh is cheap to invoke).
- *Plex vs Jellyfin shapes.* Plex needs one request per (section, path) and a
  query-string token. Jellyfin batches updates as JSON and authenticates via
  header. Two modules, distinct error shapes:
  `Plex::refresh -> Result<(), Vec<String>>` (per-pair warnings),
  `Jellyfin::refresh -> Result<(), String>` (single request, single warning).
- *Env resolution at call time.* Refresh ignores the backend with a warning
  when its `*_env` is unset, instead of erroring at config-load. Same pattern
  as the qBittorrent password and Telegram bot token.
- *Refresh targets the link-site path under the canonical category.* The
  full path is `<library_root>/<category>/<rel_path>/<name>`, since
  `linking::link_to_site` places the content at the join. Plex and Jellyfin
  both want the *content* path; pointing at the parent directory would also
  work but is more invalidation than we want.
- *Tests use the in-tree TCP mock pattern,* same as `notify::telegram` —
  no `wiremock` dep.

**Tests added (9, total 234/234):**
- `media::tests`: site_abs_paths joiner, refresh_blocking is a noop with no
  backends, noop with empty input.
- `media::plex::tests`: fan-out per (section, path) with assertions on the
  outgoing query, non-2xx becomes a warning, empty section list is a noop.
- `media::jellyfin::tests`: POSTs the batched updates with the auth header,
  empty paths is a noop, HTTP 500 surfaces.

**Outcome:**
- `cargo test --bin tql` 234/234 green.
- No new deps.
- Leg 15 (notifications + media refresh) is complete; only Leg 16 (doctor
  full checks, reload, polish) remains in the implementation plan.

## 2026-05-11 — Session: Leg 16a (`tql doctor` deep checks)

**State at start:** Leg 15c done; only Leg 16 (doctor + reload + polish) left.
`cmd/doctor.rs` only parsed config and printed a summary. `cmd/reload.rs` was
still the stub.

**Done:**
- Split Leg 16 into 16a (doctor), 16b (reload), 16c (polish — TBD).
- Added `qbit::Client::app_version` (GET `/api/v2/app/version`) as a
  post-login reachability probe.
- Rewrote `cmd/doctor.rs` end-to-end. Each invariant from DESIGN.md §7
  "doctor" is its own `Check { name, status: Ok|Warn|Fail }` collected into
  a single checklist printed at the end with an aggregate count. FAILs
  cause exit 1; WARNs are advisory.
  - **paths.seed_root / library_root**: must exist as dirs.
  - **paths.same_fs**: `MetadataExt::dev()` comparison — if these diverge
    `link(2)`/reflink will EXDEV, so this is a FAIL.
  - **paths.metadata_dir**: `<library_root>/.metadata/` — auto-created and
    write-probed (touch+remove). Same dir Leg 4 sidecar uses.
  - **trackers.root + per-tracker load + fixtures**: rebuilds the registry
    via `load_dir` and runs every fixture via `fixtures::run_all`. Reuses
    the same engine as `tql test`.
  - **qbittorrent.login + qbittorrent.version**: skips with WARN when
    `[qbittorrent]` is missing; FAILs if the password env is unset; otherwise
    spins a current-thread tokio runtime, logs in, and probes
    `app_version`. Reuses the existing `Client`.
  - **probe.notify.telegram**: `getMe` against `api.telegram.org`.
  - **probe.media.plex**: `GET <url>/identity` with `X-Plex-Token`.
  - **probe.media.jellyfin**: `GET <url>/System/Info/Public` (unauth).
  - All probes share `http_probe(name, url, header)` with a 5 s timeout —
    same budget as the §15 media refresh.

**Decisions:**
- *Status enum (Ok/Warn/Fail) over a flat `passed` flag.* Doctor's value is
  in the differentiated diagnostic — "qBittorrent unconfigured" is not the
  same as "qBittorrent unreachable", and the user should see both states
  distinctly. WARN doesn't tip the exit code.
- *Same-fs check is a FAIL, not a WARN.* Cross-device is a hard blocker for
  the entire linking subsystem (DESIGN.md §9 "no copy fallback"); soft-warning
  it would be misleading.
- *Probes are opt-in (`--probe`).* The static checks are always safe; probes
  send real HTTP to user infra and may rate-limit. Default doctor stays
  cheap and CI-friendly.
- *Tokio runtimes are spun per-section* (qBittorrent, each HTTP probe). The
  session is short-lived and we never need cross-section concurrency; one
  runtime per section keeps lifetimes trivial. A single runtime would be
  marginally faster — fine to consolidate later if doctor grows hot.
- *Re-runs the registry fresh* rather than passing a pre-built one in.
  Doctor is a cold-start sanity check; sharing state with a hypothetical
  long-running process would mask exactly the kinds of bugs doctor exists
  to catch.

**Tests added (4, total 238/238):**
- `cmd::doctor::tests::paths_ok_when_all_present_same_fs`
- `cmd::doctor::tests::paths_fail_when_seed_missing`
- `cmd::doctor::tests::trackers_check_runs_fixtures` (uses the in-tree
  `trackers/example`)
- `cmd::doctor::tests::qbittorrent_warn_when_unconfigured`

**Outcome:**
- `cargo test --bin tql` 238/238 green.
- No new deps.
- `tql doctor` is now a real preflight: filesystem, trackers, qBittorrent
  (and optionally notify + media servers).
- Leg 16b (`tql reload` PID-file signaling) and 16c (final polish + docs)
  remain.

## Leg 16b — `tql reload` PID-file + signal dispatch (2026-05-11)

**What shipped:**
- `src/pidfile.rs` — directory selection (`$TQL_RUN_DIR` → `$XDG_RUNTIME_DIR/tql/`
  → `/run/tql/` when root → `/tmp/tql-<uid>/`), atomic `write` (temp + rename),
  `read` (stale = `kill(pid,0)` returns ESRCH; stale files are pruned),
  `remove`, `send_sighup`.
- `scripting::registry::RegistryHandle` — `Arc<RwLock<Arc<Registry>>>` wrapper
  with `load()`/`swap()` so handlers acquire a snapshot per request and the
  signal task can replace the registry atomically.
- `cmd::api::run` and `cmd::mcp::run` (HTTP mode only): write `<role>.pid`
  on startup, install a tokio `SignalKind::hangup()` listener that rebuilds
  the registry via `load_dir` and `swap`s it in, plus a `shutdown_signal`
  selector on SIGINT/SIGTERM that removes the PID file before returning.
  Stdio MCP stays PID-file-free per design (trusted local pipe).
- `cmd::reload::run` — load config → validate trackers (`--skip-validate`
  to bypass) → look up `api.pid` and `mcp.pid` → deliver SIGHUP. Warns +
  exits 0 when no live server is found, exits 1 on signal delivery errors.

**Design notes / deviations:**
- *Engine is not rebuilt on SIGHUP.* The sandbox engine has no per-tracker
  state — only `load_dir`'s `compile` calls consume it. Reusing the boot
  engine keeps the swap O(scripts) and avoids re-registering host functions.
- *No two-phase commit / generation counter.* `Arc<RwLock<Arc<Registry>>>`
  ⇒ the worst inflight request sees the old snapshot, the next sees the
  new one. A request that was mid-classify against an evicted Tracker keeps
  running against its own `Arc<AST>` clone. This is the same trade-off the
  reqwest cookie jar uses and matches DESIGN.md §17's "live reload" note.
- *OpenAPI doc is rebuilt per request* implicitly: the handler calls
  `build_openapi(&registry.load(), …)` each time, so it reflects the
  current registry without further plumbing.
- *Validation in `tql reload` is opt-out.* The default is to refuse to
  signal if `<trackers_root>` itself is unusable; `--skip-validate` is
  there for the live-PID test and the rare "I know it's fine, just
  signal" case.
- *No PID file race ownership check.* A stale `<role>.pid` whose PID
  happens to be reused by another process would receive an errant SIGHUP.
  Not worth solving without a real signal multiplexer; in practice the
  unprivileged `/run/user/<uid>/` directory makes this very unlikely.

**Tests added (8, total 246/246):**
- `pidfile::tests::read_absent_returns_none`
- `pidfile::tests::write_then_read_roundtrip`
- `pidfile::tests::read_stale_pid_returns_none_and_removes_file`
- `pidfile::tests::remove_is_idempotent`
- `pidfile::tests::read_garbage_yields_parse_error`
- `cmd::reload::tests::no_running_servers_yields_zero_exit_with_warning`
- `cmd::reload::tests::stale_pid_file_is_treated_as_no_server`
- `cmd::reload::tests::live_pid_receives_sighup_and_returns_ok`
  (installs an in-process SIGHUP handler and verifies delivery)

**Outcome:**
- `cargo test --bin tql` 246/246 green.
- New deps: `libc = "0.2"`; tokio gains the `signal` feature.
- Leg 16c (final polish: README + DESIGN cross-links, bounded
  `[reconcile] parallelism`) remains.

## 2026-05-11 — Session (Leg 16c-1)

**State at start:** Leg 16b landed; 246/246 green. Leg 16c remained as a
polish bucket containing three loose items (docs, reconcile parallelism,
Cargo metadata).

**Done:**
- Split Leg 16c into 16c-1 (parallelism) and 16c-2 (docs + metadata).
- Implemented bounded reconcile parallelism. `cmd/reconcile.rs` triages
  torrents into `Slot::Skip|Run` up front, then dispatches the Run set
  onto `tokio::task::spawn_blocking` throttled by a
  `tokio::sync::Semaphore` of size `cfg.reconcile.parallelism.max(1)`.
- Outcomes collected in input order so the printed summary stays
  deterministic regardless of finish order.
- Added one test (parallelism=2 over 3 torrents) verifying all three
  link successfully.

**Decisions:**
- Kept the existing current-thread tokio runtime. `spawn_blocking` runs
  on a dedicated blocking pool regardless of flavor, so we get real
  parallelism without rebuilding the whole runtime to multi-thread.
- `tokio::sync::Semaphore` was preferred over rolling a counter because
  it's already pulled in via tokio's `sync` feature (no new deps).
- Per-hash flock inside `post_process::process_with_cfg` continues to
  guarantee single-writer per sidecar; the semaphore only caps global
  fan-out.

**Outcome:**
- `cargo test --bin tql` 247/247 green.
- No new deps.
- Leg 16c-2 (README + DESIGN cross-links, Cargo metadata pass) remains.

## 2026-05-11 — Session (Leg 16c-2)

**State at start:** 16c-1 landed (bounded reconcile parallelism). 16c-2 was
the remaining polish: docs + Cargo metadata. No `README.md` in the repo.

**Done:**
- Wrote `README.md`: blurb, NixOS build/test recipe, subcommand table with
  DESIGN.md section cross-links, config search order, tracker dir layout,
  license.
- `Cargo.toml`: added `repository`, `readme`, `keywords`, `categories` so
  the crate manifest is publish-ready (we are not actually publishing).
- `cargo check` clean; no test changes (still 247/247 from 16c-1).

**Decisions:**
- Kept the README short on purpose — DESIGN.md is the source of truth and
  the table just hyperlinks into it. Avoids the README drifting out of
  sync with section numbers.
- Didn't bump version past `0.0.1` — there's no release cadence yet.

**Outcome:**
- All planned legs (1 → 16c-2) are now complete.

## 2026-05-11 — Session: Leg 17 (Nix flake)

**State at start:** all 16 planned legs done; toolchain accessed only via
ad-hoc `nix shell nixpkgs#cargo nixpkgs#rustc nixpkgs#gcc -c …` per CLAUDE.md.

**Done:**
- Wrote `flake.nix` (single `nixpkgs-unstable` input, manual `forAllSystems`
  across the four common systems — no `flake-utils` dep).
- `devShells.default` ships cargo + rustc + rustfmt + clippy + gcc + pkg-config.
- `formatter = nixpkgs-fmt` so `nix fmt` works.
- Committed `flake.lock` (deterministic builds).
- Rewrote CLAUDE.md toolchain section to lead with `nix develop --command …`;
  kept the legacy `nix shell` invocation as a fallback paragraph.

**Decisions:**
- Skipped `flake-utils` — adds an input for ~10 lines of saved boilerplate
  that is trivial to write inline.
- No `packages.<system>.default = rustPlatform.buildRustPackage` yet. That
  would belong in a separate leg (and needs `cargoHash` maintenance which
  is friction without value while we're not publishing). Devshell is the
  contributor-facing surface for now.
- `rustls` removes the need for OpenSSL, so no `openssl` / `pkg-config`
  hard requirement — `pkg-config` included as cheap insurance for future
  C-deps.

**Outcome:**
- `nix develop --command cargo check` succeeds (10.8 s on a warm cache after
  the first download of clippy/rustfmt/gcc/pkg-config closures).
- `cargo`/`rustc` versions inside the shell: 1.94 (matches the previous
  ad-hoc `nix shell` path — same nixpkgs channel).
- No source code or Cargo.toml changes; 247/247 tests untouched.

## 2026-05-11 — Session: Leg 18 (nix build)

**State at start:** Leg 17 left `flake.nix` with a devshell only; packaging
explicitly deferred. All 247 tests green.

**Done:**
- Added `packages.<system>.default` to `flake.nix` (four systems via the
  existing `forAllSystems` helper) using `rustPlatform.buildRustPackage`.
- Lockfile-based vendoring (`cargoLock.lockFile = ./Cargo.lock`) — no
  `cargoHash` to maintain.
- `doCheck = false` so the Nix sandbox doesn't run the test suite (it spins
  up TCP mocks the sandbox can't service); cargo tests remain the source of
  truth via the devshell.
- `meta` set with dual MIT/Apache-2.0, mainProgram, homepage, platforms.
- README's Build section reworked to lead with `nix build` (install path) and
  keep `nix develop --command cargo …` for iteration.

**Verification:**
- `nix build .#default` succeeded (warm cache, full rust dependency rebuild
  the first time; subsequent rebuilds will hit the Nix store).
- `./result/bin/tql --help` lists all subcommands.
- Binary size: 11 MB stripped release.

**Decisions:**
- No `apps.default` flake output — `mainProgram = "tql"` already lets
  `nix run` discover the binary, and an explicit `apps` attr would be
  duplicate plumbing.
- Did not promote pkg-config to `buildInputs` (it's a build-time helper,
  not a link-time dep) — kept it in `nativeBuildInputs` only.

**Outcome:**
- Flake now produces installable package + devshell. All 247 tests still
  green (untouched).

---

## Leg 19 — NixOS module (DONE 2026-05-11)

**Goal:** ship `nixosModules.default` so the systemd units listed in
DESIGN.md §16 (api, mcp HTTP, reconcile timer, notify-flush timer) can be
declaratively enabled by downstream NixOS hosts.

**Done:**
- New `nix/module.nix` with `services.tql.*` options: `enable`, `package`,
  `user`/`group`, `configFile` XOR `settings` (rendered via
  `pkgs.formats.toml`), `environmentFile` (for secrets), `readWritePaths`
  (required because `ProtectSystem=strict`), and per-unit `api`, `mcp`,
  `reconcile`, `notifyFlush` blocks each with `enable`, `extraArgs`, and
  (where applicable) `listenAddress` / `onCalendar`.
- Hardened systemd defaults across every unit: `NoNewPrivileges`,
  `Protect{System,Home,KernelTunables,KernelModules,KernelLogs,ControlGroups}`,
  `Restrict{Namespaces,SUIDSGID,AddressFamilies}`, `LockPersonality`,
  `MemoryDenyWriteExecute`, `SystemCallArchitectures=native`,
  `PrivateTmp`, `StateDirectory=tql`.
- `TQL_CONFIG` env var points each unit at the rendered/declared config
  file; `EnvironmentFile` (optional) injects secrets.
- Long-running services (`api`, `mcp`) get `Restart=on-failure` and
  `wants/after=network-online.target`. Reconcile + notify-flush are
  `Type=oneshot` paired with `.timer` units (defaults: every 10 min /
  every 2 min; `Persistent=true` to catch up after downtime).
- `flake.nix` exposes `nixosModules.default` (auto-defaulting
  `services.tql.package` to `self.packages.${system}.default`) and a
  `nixosModules.tql` alias.

**Verification:**
- `nix flake check --no-build` passes including the new NixOS module
  outputs.
- Module options instantiate cleanly with both `configFile` and
  `settings` paths; mutual-exclusion assertion fires when both are set.
- Disk-full on the host blocked a full VM test, but module evaluation
  succeeds during `nix flake check` (it walks the module options).

**Decisions:**
- Module file lives under `nix/module.nix` (not in `flake.nix` itself)
  so non-flake imports (`import ./nix/module.nix`) keep working.
- `pkgs.formats.toml` rather than a hand-rolled `lib.generators.toTOML`
  — gives users the full TOML escape semantics for free.
- Defaulted MCP listener to `127.0.0.1:7878` (loopback) — operators opt
  into exposing it.
- Did not add a NixOS VM test under `checks.<system>` yet: the existing
  test suite already covers behavior, and a VM test would balloon CI for
  marginal benefit. Future leg if needed.

**Outcome:**
- Downstream hosts can `imports = [ tql.nixosModules.default ]` and flip
  `services.tql.enable = true` plus per-unit toggles. All 247 cargo
  tests still green (no Rust code touched).

## Leg 20 — NixOS VM test under checks.<system> (2026-05-11)

**Goal:** end-to-end coverage for the leg-19 NixOS module by booting an
actual VM, enabling `services.tql.enable = true`, and verifying the
`tql-api` unit comes up and answers `/health`. Leg 19 explicitly punted
on this; revisiting now that the module is stable.

**Changes:**
- New `nix/test-module.nix`: a `pkgs.testers.runNixOSTest` derivation
  wiring the module on a single `machine` node with `api.enable = true`,
  `paths.{seed,library,trackers}_root` under `/var/lib/tql/...`,
  `api.addr = 127.0.0.1:8080`, and `systemd.tmpfiles.rules` to pre-create
  the per-path subdirs (StateDirectory only creates `/var/lib/tql`
  itself). Test script waits for the unit + open port, then curls
  `/health` (expects `ok`) and `/trackers` (expects empty `[]`), and
  verifies `tql --help` works from the system PATH.
- `flake.nix`: new `checks = forAllSystems (pkgs: optionalAttrs
  pkgs.stdenv.isLinux { nixos-module = import ./nix/test-module.nix
  {...}; })`. Gated on Linux because nixosTest needs KVM/QEMU.

**Decisions:**
- `pkgs.testers.runNixOSTest` (not the deprecated `pkgs.nixosTest`).
- `tmpfiles.rules` rather than an ExecStartPre — services start cleanly
  on boot without test-script handholding.
- Test script uses raw `curl | grep` rather than parsing JSON — keeps
  the test dep-free (just curl, already pulled in).

**Verification:**
- `nix flake check --no-build`: passes; the new `checks` output
  evaluates.
- `nix build .#checks.x86_64-linux.nixos-module`: VM boots, tql-api
  starts in ~18 s, all three `must succeed` steps pass, total test
  script time ~21 s.
- No Rust code touched: 247/247 cargo tests still green from Leg 16c-1.

## Leg 21 — GitHub Actions CI (2026-05-11)

**Goal:** automated verification on every push/PR — fmt, tests, flake
evaluation, release build. Until now everything ran only on the dev's
laptop.

**Changes:**
- `.github/workflows/ci.yml`: single `ubuntu-latest` job, concurrency
  group cancels superseded runs on the same ref. Steps: checkout →
  `cachix/install-nix-action@v27` (unstable, flakes on) →
  `DeterminateSystems/magic-nix-cache-action@v8` → `nix develop
  --command cargo fmt --check` → `nix develop --command cargo test
  --bin tql` → `nix flake check --no-build` → `nix build .#default`.
- One-time `cargo fmt` sweep across `src/` (27 files) so the new
  `cargo fmt --check` gate is green from the first run. Pure
  whitespace.

**Decisions:**
- `nix flake check --no-build`, not the full `nix flake check`. The
  Leg 20 VM test needs KVM + ~20 s of boot time per run; defer to a
  later leg if/when we want it on every PR. Eval-only still catches
  module breakage.
- `magic-nix-cache-action` instead of pinning `cachix.org` — it's
  zero-config and uses GitHub's own action cache, which is what we
  want for a public repo with no Cachix account.
- No clippy step yet. Adding `-D warnings` would either rubber-stamp
  the current clean state or surface noise we haven't audited;
  punting until we want it as a forcing function.
- Skipped a separate `nix build` matrix across systems
  (x86_64-darwin etc.) — GH-hosted macOS minutes are 10x and our
  `flake.nix` already declares the four standard systems; one Linux
  build is enough signal for now.

**Outcome:**
- CI workflow committed; will exercise itself on push. 247/247
  cargo tests still green locally. No Rust source behavior changed.

## Leg 22 — `tql sidecar show <hash>` (2026-05-11)

**Goal:** retire the Leg-1 stub for `tql sidecar show`. DESIGN §7 calls
for a one-shot inspection command that prints the per-torrent sidecar
JSON for a given info hash.

**Changes:**
- `src/cmd/sidecar_show.rs`: rewritten. `Args` adds an optional
  `--config <PATH>` flag (same shape as `tql test`). `run` loads the
  config and forwards to a private `show(&Config, &str, &mut impl
  Write)` so tests can assert on the printed JSON without spawning a
  subprocess. Reuses `sidecar::sidecar_path` + `sidecar::read` — the
  shared `flock` path is the same one post-process / reconcile take, so
  there's no risk of reading a half-written file.

**Decisions:**
- Pretty-print via `serde_json::to_string_pretty`. Round-trippable;
  matches the on-disk format `sidecar::write` produces.
- Missing sidecar = exit 1 with a path in the error, *not* exit 0 with
  empty stdout. The user asked about a specific hash; "no record"
  deserves a non-zero status so it composes with shell pipelines.
- Malformed sidecar = exit 1 too. We don't try to dump the raw bytes
  on parse failure; the caller can `cat` the file themselves if they
  want the corrupt content.
- No `--raw` / `--no-pretty` / `--metadata-dir` knobs. YAGNI; the
  sidecar layout is fixed by §14 and the JSON is short enough.

**Verification:**
- 4 new tests in `cmd::sidecar_show::tests`: happy-path round-trip,
  missing sidecar, malformed JSON, `Origin::PostProcess` snake-case
  serialization.
- `cargo fmt --check` clean. `cargo test --bin tql` → 251 passed
  (+4 from Leg 16c-1 / Leg 21's 247 baseline).

**Outcome:**
- `tql sidecar show <hash>` now functional. `tql link {add,remove}` are
  the only remaining stubs from the original Leg-1 dispatch table.

## Leg 23 — `tql link {add,remove}` (2026-05-11)

**Goal:** retire the last two Leg-1 stubs. DESIGN §7 calls for manual
add/remove of a `link:` tag — updates qBittorrent tags then triggers a
single-torrent reconcile.

**Changes:**
- `src/qbit/mod.rs`: new `Client::add_tags`/`remove_tags` plus a private
  `set_tags(path, hashes, tags)` helper. POST form
  `hashes=<a|b>&tags=<csv>` to `/api/v2/torrents/{add,remove}Tags`.
  Non-2xx → `AddFailed` (reused error variant). qBittorrent answers 200
  with an empty body on success — no body check needed.
- `src/cmd/link.rs`: new shared module exposing `Op::{Add,Remove}` and
  `run(op, hash, path, config)`. Pipeline: load config → offline-validate
  the tag string via `paths::parse_link_tag(_, None)` → login to
  qBittorrent → call `add_tags`/`remove_tags` → re-fetch via
  `torrents_info` → re-validate with the canonical category (catches the
  StartsWithCategory producer rule) → synthesize `post_process::Args`
  from the freshly fetched `TorrentInfo` → reuse
  `post_process::process_with_cfg` for the actual link-site mutation.
- `src/cmd/{link_add,link_remove}.rs`: thin wrappers that parse clap args
  and delegate to `link::run`. Both gain an optional `--config <PATH>`
  flag (mirrors `tql test`, `tql sidecar show`, etc.).
- `src/cmd/mod.rs`: register the new `link` module.

**Decisions:**
- Two-phase validation. The `StartsWithCategory` rule needs the
  canonical category, which we don't know until we fetch from
  qBittorrent. Validating *before* the qBittorrent call (with category =
  None) catches the cheap failures up front; re-validating after the
  fetch catches the category-specific one. Worst case we add/remove a
  tag and then bail out — qBittorrent's tag is the user's anyway, and a
  follow-up `tql reconcile` would mop up.
- Re-use `post_process::process_with_cfg` rather than duplicating the
  link/unlink + sidecar diff logic. Same code path the qBittorrent hook
  takes — DESIGN.md explicitly says this command triggers a
  single-torrent reconcile.
- A torrent without a category aborts with exit 1. Tracker-qualified
  layout doesn't make sense otherwise; better to surface the mismatch
  loudly than silently apply only the tag side-effect.
- `apply_op` (pure tag-set helper) kept under `#[cfg(test)]` only —
  unused in production because qBittorrent does the merging server-side,
  but the tests document the semantics.

**Verification:**
- 4 new unit tests in `cmd::link::tests` covering add/remove
  idempotence + appending.
- `cargo test --bin tql` → 255 passed (+4). `cargo fmt --check` green
  after letting rustfmt collapse a couple of long literals.

**Outcome:**
- All originally-stubbed subcommands are now functional. Dispatch table
  from Leg 1 is fully implemented.

## Leg 24 — End-to-end test for `tql link add` (2026-05-11)

**Plan:** retire one Leg-23 follow-up — drive `cmd::link::run(Op::Add)`
through a stubbed qBittorrent (login + addTags + torrents_info) and
assert the post-process side effects (hardlink + sidecar).

**Decisions:**
- Inlined the HTTP mock (`spawn_mock`/`ok_text`/`ok_json`/`TempDir`) into
  `cmd::link::tests` rather than extracting a shared `dev-utils` module.
  The `cmd::reconcile::tests` and `cmd::post_process::tests` modules
  already carry their own copy each; adding a third copy keeps cohesion
  high (each test module reads top-to-bottom) and avoids designing a
  reusable harness for one extra call site. If we add a fourth, that's
  when we extract.
- Made the password env-var name PID-suffixed
  (`TQL_TEST_LINK_PW_<pid>`) to avoid colliding with the reconcile tests'
  hard-coded `TQL_TEST_QB_PASSWORD` if cargo runs both modules in
  parallel.
- Asserted on **request ordering** (login < addTags < info), not just
  presence — `link::run` must mutate qBittorrent *before* re-fetching
  the canonical info, otherwise the new tag would be missing and the
  StartsWithCategory revalidation would fail open instead of catching
  a real bug.
- Mock returns the canonical info with `link:Cat/Sub` already in
  `tags` so the simulated post-fetch state matches what qBittorrent
  would actually return after a successful addTags. Category
  `tracker.tld` was chosen so the path `Cat/Sub` doesn't trip the
  `StartsWithCategory` rule.

**Outcome:**
- 1 new test (`link_add_end_to_end_creates_link_and_sidecar`); 256/256
  total. No new deps. The `apply_op` micro-tests stay (they cover the
  pure helper used by future `tql link diff` style commands).
- `link remove` end-to-end remains unwritten — leaving as future work
  because (a) it needs a pre-existing sidecar + linked tree fixture
  and (b) the diff path through `post_process` is already covered by
  `cmd::post_process::tests::removes_stale_sites`.

## 2026-05-11 — Session: Leg 25 (`link remove` E2E)

**Picked up:** the explicit open follow-up from Leg 24 — write the
symmetric end-to-end test for `cmd::link::run(Op::Remove, ...)`. All
other ladder rungs (publish to crates.io, rmcp swap, SSE, VM-in-CI under
KVM) are larger, looser, or external; this one is bounded and reuses
Leg-24 scaffolding verbatim.

**Done:**
- `src/cmd/link.rs::tests::link_remove_end_to_end_unlinks_and_updates_sidecar`.
- Seeds `<lib>/tracker.tld/Cat/Sub/Book` as a hardlink to the seed file
  and writes a sidecar with one `link_sites[]` entry — the "world after
  a prior `link add`" state.
- Mock: login → `Ok.`, `removeTags` → HTTP 200, `torrents_info` → JSON
  with `tags: ""` (canonical post-remove state).
- Asserts: request order (login < removeTags < info), link target gone,
  `Cat` parent pruned by `linking::unlink_site`, sidecar `link_sites`
  empty after the run.

**Decisions / notes:**
- Pre-creating the linked tree + sidecar (rather than running `Op::Add`
  first) keeps the test single-purpose. Adding-then-removing in one
  test would also work but doubles the mock state machine and would
  duplicate Leg-24 coverage.
- `Sidecar` has no `Default` impl — explicit field initializers for
  `last_applied_at` and `warnings` instead of `..Default::default()`.
- Env-var name PID-suffixed (`TQL_TEST_LINK_RM_PW_<pid>`) so it can't
  race against Leg-24's `TQL_TEST_LINK_PW_<pid>` if the two tests share
  a process (cargo's default test runner does).

**Outcome:**
- 1 new test; 257/257 total. No new deps.

## 2026-05-11 — Leg 26: Clippy gate in CI

**Picked up:** future-work item not in the prior ladder — we had a
fmt-only lint gate but no `cargo clippy` step, and a fresh
`cargo clippy --bin tql --tests -- -D warnings` surfaced 5 real lints
plus 4 `result_large_err` complaints. Wiring clippy now keeps the bar
honest as future legs land.

**Done:**
- `Cargo.toml` grows a `[lints.clippy]` table allowing
  `result_large_err`. These error enums (`SidecarError`, `LinkError`,
  `FixtureFailure`, `FixtureFailureKind`, axum/MCP `Response`) carry
  diagnostic state by design; boxing them is friction without payoff.
- Fixed in source:
  - `src/cmd/mod.rs` — dropped the dead `unimplemented` helper (every
    subcommand has real `run` now, post-Leg 23).
  - `src/paths.rs:67` — `trim_end_matches(['.', ' '])` instead of a
    closure (clippy::manual_pattern_char_comparison).
  - `src/paths.rs:327` — `"ñ".repeat(150)`.
  - `src/paths.rs:423` — `vec!["a"; MAX_PATH_COMPONENTS + 1].join("/")`.
  - `src/cmd/link.rs:194` — `.filter(...).cloned()` instead of
    `.cloned().filter(...)`.
- `.github/workflows/ci.yml` gains a `cargo clippy --bin tql --tests --
  -D warnings` step between fmt and test.

**Outcome:**
- `nix develop --command cargo clippy --bin tql --tests -- -D warnings`
  clean. 257/257 tests still green. No new deps.

---

## 2026-05-11 — Leg 27: `tql sidecar gc`

Implements the `tql sidecar gc` subcommand listed in DESIGN.md §17 Open
Questions ("sidecars for torrents removed from qBittorrent … separate
command, not automatic"). Closes the last category of orphaned-state
maintenance.

**Approach:**

- New `src/cmd/sidecar_gc.rs`. `Args { dry_run, config }`.
- Pipeline: load config → require `[qbittorrent]` → tokio current-thread
  runtime → login → `torrents_info(default query)` → `BTreeSet<String>`
  of lowercased known hashes.
- Disk pass: scan `<library_root>/.metadata/`, skip dotfiles (lock
  files start with `.<hash>.json.lock`), keep `*.json`, lowercase the
  hash for case-insensitive comparison.
- Orphan path: `sidecar::read` under shared lock → `linking::unlink_site`
  per `LinkSite.resolved_path` with `<library_root>/<category>/` as the
  stop boundary (same boundary as post-process). On per-site error, set
  `had_err` and *leave the sidecar in place* so a re-run can retry — if
  we deleted the sidecar we'd forget the resolved paths. On full
  success, remove the sidecar JSON and best-effort delete its adjacent
  `.lock` file.
- `--dry-run` prints `would unlink <path>` / `would remove sidecar
  <path>` lines and leaves the filesystem untouched; the counters still
  reflect what *would* happen.
- Exit code: 1 on any per-orphan error or qBittorrent connection
  failure, 0 otherwise. Summary printed to stderr:
  `scanned, kept, orphans, removed, sites unlinked, errors`.
- Wired into `main.rs` as `SidecarAction::Gc`. The factored
  `gc_with_known(&cfg, &known, dry_run)` is the testable core — no
  qBittorrent mock needed in tests because the known-hash set is the
  injection point.

**Tests (6 new):** orphan removed + parent dirs pruned; known hash
kept untouched; dry-run leaves fs intact but counters reflect intent;
missing `.metadata/` is a no-op; dotfiles + non-`.json` filtered out;
mixed-case hash matches lowercase qBittorrent set. 263/263 green
(+6). No new deps. fmt + clippy clean.

**Surprises:**

- DESIGN.md §17 flags this as an *open question* rather than spec, so
  the exit-code policy and the "leave sidecar on partial failure" rule
  are choices made here. Documented above for future regression.

---

## 2026-05-11 — Leg 28: `tql doctor --json`

**Picked up:** the full set of DESIGN.md §7 subcommands is implemented;
all 27 prior legs are closed. Operational notes in §16 lean on
`tql doctor` ("doctor after every config or manifest change") but the
existing output is shaped for a human terminal. Adding a JSON mode is a
small, surgical operational polish — useful for CI gates, systemd
post-deploy hooks, monitoring agents.

**Sidebar — crates.io status:** ran `cargo publish --dry-run --allow-dirty`
to gauge readiness. It packages cleanly (713 KiB, 192.9 KiB compressed)
but reports `crate tql@0.0.1 already exists on crates.io index`. The
name is squatted (or there's an unrelated `tql`); resolving requires a
naming/owner decision out of scope here. Noted in PLAN.md future-work
so it doesn't drift off the radar.

**Done:**

- `cmd/doctor.rs::Args` grows `json: bool` (`--json`).
- `finish(checks, json)` now splits into helpers:
  - `tally(&[Check]) -> (fails, warns)` counts once.
  - `status_message(&Status) -> &str` collapses the per-variant
    `Ok|Warn|Fail` body extraction so the human path matches the JSON
    path (no risk of drift on string formatting).
  - `render_json(&[Check], fails, warns) -> serde_json::Value` shapes
    the document.
- JSON shape (stable):
  ```
  {
    "checks": [{"name": "...", "status": "ok|warn|fail", "message": "..."}, ...],
    "summary": {"total": N, "ok": N, "warn": N, "fail": N},
    "exit_code": 0|1
  }
  ```
  `exit_code` is echoed inside the document so log/JSONL consumers that
  don't observe process status can still gate on it.
- Exit-code policy is **unchanged** — still 1 on any FAIL, 0 otherwise.

**Tests (2 new):**
- `render_json_shape_and_summary` — three-check checklist, asserts per-
  status strings, summary counts, and `exit_code = 1`.
- `render_json_exit_zero_when_no_failures` — one-check OK list,
  asserts `exit_code = 0` and `summary.fail = 0`.

**Outcome:** 265/265 tests green (+2). `cargo fmt --check` and
`cargo clippy --bin tql --tests -- -D warnings` both clean. No new deps.

**Surprises:**

- `serde_json` is already in the tree (used by sidecar, api, mcp,
  openapi) so the JSON serializer was free. Resisted the urge to grow a
  shared `Status::tag()` helper — it would be one-shot and the inline
  match reads fine.

## 2026-05-11 — Session 30 (Leg 29)

**State at start:** Leg 28 closed; PLAN.md "Future work" section was the only
pending pointer. Audit of DESIGN.md against the source turned up a hole:
DESIGN.md §8 says "For URL fetches, the Rust core consults a per-tracker
credential table in config (cookies or auth headers) to authenticate to the
tracker site." `TrackerCreds { cookie_env, auth_header_env }` was wired into
`Config` since Leg 2 but never *read*; URL sources were passed to qBittorrent
verbatim. Picked it up as Leg 29.

**Done:**
- New `src/fetch.rs`: `FetchError` (`InvalidUrl|MissingEnv|Http|Status{u16,
  body}`), `has_creds(&TrackerCreds) -> bool`,
  `async fn fetch_torrent_with_creds(url, &TrackerCreds) -> Result<Vec<u8>,
  FetchError>`. Builds a single-shot `reqwest::Client` with a 30 s timeout;
  rejects non-`http(s)` schemes up front; populates `Cookie` and/or
  `Authorization` from the configured env vars (verbatim — cookie value goes
  in raw, header value carries any `Bearer ` / `Token ` prefix the tracker
  expects). Non-2xx surfaces `Status { status, body }` with the body text
  captured for diagnostics.
- New `cmd::cli::resolve_torrent_source(&Config, tracker_name, source, kind)`
  is the shared resolver: when `kind == Url` *and* `cfg.trackers[tracker_name]`
  has `cookie_env` or `auth_header_env`, it fetches with creds and returns
  `TorrentSource::File { filename = last URL path segment, bytes }`. Otherwise
  it delegates to the existing `build_torrent_source` (magnet/URL passthrough
  or filesystem read). `ResolveError` newtype wraps the two underlying error
  enums so call sites get one `Display` to print.
- Three call-site swaps:
  - `cmd/cli.rs::dispatch` — moved the source-resolution call inside the
    tokio runtime block (it's async now). Tokio runtime build moved earlier
    so we can `block_on` the resolver before logging in to qBittorrent.
  - `cmd/api.rs` `add` handler — handler is already in tokio land,
    `await`s the resolver directly. Dropped the now-unused
    `build_torrent_source` import.
  - `cmd/mcp.rs::tools_call` — same shape as the API handler.
- `mod fetch` declared in `src/main.rs`.

**Tests (6 new in `fetch::tests`):**
- `has_creds_detects_cookie_or_header` — pure unit.
- `fetches_with_cookie_header_from_env` — TcpListener mock asserts
  `Cookie: mam_id=secret123` is on the wire and the response body is
  returned verbatim.
- `fetches_with_authorization_header_from_env` — same but for the
  `Authorization: Bearer …` header.
- `missing_env_var_is_an_error` — `cookie_env` pointing at an unset name
  → `MissingEnv(_)` (no network call attempted).
- `non_2xx_surfaces_status_and_body` — mock returns 403 with a body;
  asserts `FetchError::Status { status: 403, body }` with the body
  captured.
- `rejects_non_http_url` — passing a `magnet:` URI is `InvalidUrl`
  rather than crashing reqwest.

Env var names are PID + line!() suffixed to keep parallel test runs from
clobbering each other (the Leg 24/25 pattern).

**Outcome:** 271/271 green (+6). `cargo fmt --check` and
`cargo clippy --bin tql --tests -- -D warnings` both clean. No new deps —
reqwest+tokio were already in the tree.

**Decisions / surprises:**
- Kept `build_torrent_source` as the sync primitive; the async resolver
  layered on top means tests that only need the sync path stay sync.
- Filename for the upload uses the last URL path segment so qBittorrent
  shows `torrents.php` for sites that use a query-string id; not pretty
  but harmless. A more polished version could parse `Content-Disposition`,
  but that's a follow-up.
- Clippy flagged `path_segments().last()` (DoubleEndedIterator) — switched
  to `next_back()` per the lint's `try` suggestion.
- The credential is read from env on every fetch, so rotating `MAM_COOKIE`
  takes effect on the next request without `tql reload`.

## 2026-05-11 — Session: Leg 30

**State at start:** Legs 1–29 all DONE. PLAN.md's tail listed "future work
remains open-ended" with a handful of suggestions; nothing actionable was
queued. 271/271 tests green.

**Done:** Leg 30 — `tql sidecar list` (`--json` flag, sorts by hash, gracefully
degrades on malformed sidecars, returns empty on missing `.metadata/`).
5 new unit tests; 276/276 green.

**Decisions:**
- Picked `sidecar list` over the other open candidates (rmcp swap, SSE,
  NixOS-VM-in-CI, crates.io publish) because it's a small, self-contained
  operator-facing affordance that closes the gap between `sidecar show`
  (single-hash lookup) and `sidecar gc` (which already scans the whole
  metadata dir but doesn't expose what it sees). Same scan/filter pattern
  as `sidecar_gc::gc_with_known`.
- Malformed sidecars produce a stub entry (hash only, empty cat/name)
  rather than being silently dropped. The operator should *see* the broken
  rows so they can `tql sidecar show <hash>` to dig in.
- `--json` emits a pretty-printed array (matching `sidecar show`'s
  pretty-printed object). Tooling that wants compact JSON can pipe to `jq -c .`.

## 2026-05-11 — Leg 31 — `tql sidecar gc` qBittorrent mock e2e

Mirrors the pattern from Legs 24/25 (`tql link {add,remove}` e2e). One new
test in `cmd::sidecar_gc::tests` drives `do_run` against a TcpListener mock
that answers `login` (sets a session cookie) and `torrents_info` (returns
`[]` so every on-disk sidecar becomes an orphan). Asserts request order,
summary counters, sidecar deletion, link-target removal, and parent-dir
pruning up to `<library_root>/<category>/`.

Notes:
- The mock infrastructure (`spawn_mock`, `ok_text`, `ok_json`,
  `TempDir`) is now inlined in three places — `cmd::link::tests`,
  `cmd::reconcile::tests`, `cmd::post_process::tests`, and now
  `cmd::sidecar_gc::tests`. Worth extracting into a `#[cfg(test)]
  mod testutil` in a future cleanup leg — not done here to keep this
  change surgical.
- Password env-var name is PID-suffixed (`TQL_TEST_GC_PW_<pid>`) to
  avoid cross-test races; cleaned up in the same test via `remove_var`.
- Initial `cargo test` build failed with a missing `Path` import in the
  new helper signature; fixed by adding `use std::path::Path;` to the
  e2e block.

## 2026-05-11 — Session 32 (Leg 32)

**State at start:** 277 tests green after Leg 31. The Leg-31 note flagged
the `spawn_mock` / `ok_text` / `ok_json` triple-duplication in
`cmd/{link,reconcile,sidecar_gc}.rs` as "left for a future cleanup".

**Done:**
- New `src/test_http.rs` (`#![cfg(test)]`) hosting the canonical
  `spawn_mock`, `ok_text`, `ok_json`. Registered in `main.rs` as
  `#[cfg(test)] mod test_http;`.
- Removed the in-tree copies from `cmd/{link,reconcile,sidecar_gc}.rs`
  and replaced them with `use crate::test_http::{...};`.
- Trimmed now-unused std imports (`io::{Read, Write}` collapses to just
  `io::Write`, `TcpListener`, `thread`, `AtomicBool` go away; `Arc` and
  `Ordering` kept where the surrounding tests still use them
  explicitly).

**Decisions:**
- Did not migrate `qbit::mod`, `fetch`, `notify::telegram`,
  `cmd::notify_flush`, `media::plex`, `media::jellyfin`. Each one has a
  *slightly* different handler signature (`Fn(&str) -> Vec<u8>`,
  different captured state, status-only stub, etc.) — subsuming them
  would need either generics or multiple variants and would expand the
  blast radius without buying much. Filed under "future cleanup" again
  rather than dragging it into this leg.
- Kept `test_http` as a top-level module instead of nesting under
  `cmd::` because the migration candidates already span `cmd::` and
  `qbit::` / `notify::` / `fetch` — a top-level home is the obvious
  next step.

**Outcome:**
- 277/277 tests still green. `cargo fmt --check`, `cargo clippy --bin
  tql --tests -- -D warnings` both clean.
- Net diff: −150-ish duplicated lines across three test modules,
  +95 lines for the shared `test_http.rs`.

**Notes for future sessions:**
- Migrating the remaining six modules to `test_http` would be a tidy
  follow-up leg if/when their handler shapes converge.

## 2026-05-11 — Leg 33: info_hash for credentialed URL fetches

`cli::build_ack` was selecting the info_hash strategy on the original
`SourceKind`. After Leg 29, a `Url` source with per-tracker credentials gets
fetched into memory and uploaded as a file, but the kind reported to
`build_ack` stays `Url` — so the ack always reported `info_hash: null`
even though the bytes were right there.

Switched `build_ack` to a bytes-first policy: if `torrent_bytes` is `Some`,
compute the hash from them; otherwise fall back per kind (magnet btih for
magnets, null for File/Url-without-bytes). Behavior is unchanged for the
three pre-existing paths:
- Magnet with no bytes: `magnet_btih(source)` (same as before).
- File with no bytes: `null` (same as before).
- Url with no bytes (uncredentialed passthrough): `null` (same as before).

New behavior:
- Url with bytes (credentialed fetch): real info_hash now reported.

Touched files: `src/cmd/cli.rs` (one block in `build_ack`, one new test).
Tests: 278/278 green (+1).

## 2026-05-11 — Session 35 (Leg 34)

**State at start:** Leg 33 closed the credentialed-URL info_hash gap;
PLAN.md's open futures list (crates.io, rmcp, SSE, KVM-in-CI) didn't have
a session-sized item. Picked a fresh operational tool instead.

**Done:**
- Leg 34: `tql sidecar verify` — cross-checks each sidecar against the
  filesystem (content_path exists; every link_sites[i].resolved_path
  exists; for !is_directory, dev/ino matches content_path).
- New `src/cmd/sidecar_verify.rs`, wired as `SidecarAction::Verify` in
  main.rs. 8 unit tests cover the happy path + each issue kind + JSON
  shape + directory existence-only. 286/286 green.

**Decisions:**
- Directory torrents only get existence-checked. Recursive per-file
  inode comparison is doable but expensive on large bundles and
  out-of-scope for this leg — the sidecar's content_path is the *root*
  of a structurally-replicated tree, and `link_to_site` already enforced
  the invariant at create time. A future leg can layer on a `--deep`
  flag if operators want it.
- Issue ordering inside an entry mirrors check order
  (`missing_content` first, then per-site issues). Stable for
  diff/regression tests.
- `Issue::ReadError` carries the formatted error message verbatim rather
  than a structured enum — `sidecar::SidecarError` is rich enough that
  the message is already actionable, and structured error data on top
  of structured issue data would push JSON consumers toward a Display
  pass anyway.
- Plain output is one line per issue (not per sidecar). Operators using
  `tql sidecar verify | grep missing_resolved` get exactly the broken
  rows; a sidecar with three issues prints three lines. Sidecars with
  no issues collapse to `<hash>  ok` so the table still shows
  everything that was scanned.
- Exit code policy mirrors `tql doctor`: 1 on any sidecar with at least
  one issue, 0 otherwise. JSON output echoes nothing extra — the
  summary block is enough.

**Notes for future sessions:**
- No new deps. `std::os::unix::fs::MetadataExt` was already in use in
  `linking.rs` / `cmd/doctor.rs` / `cmd/reconcile.rs`.
- The directory existence-only stance leaves the door open for a `--deep`
  mode (recursive structural+inode walk reusing `linking::compare_trees`)
  if operators report drift inside bundles.

## 2026-05-11 — Session 35

**State at start:** Leg 34 landed; `tql sidecar verify` shipped with both
human + `--json` output. PLAN.md had no pending leg.

**Done:**
- Leg 35: `tql sidecar gc --json`. Introduced `Report { summary, entries }`,
  `Entry`, `EntryStatus`. Collapsed `gc_with_known` into the new
  `gc_with_known_detailed(quiet)` (the wrapper would have been dead code in
  release builds — clippy `-D warnings` rejected it, so I migrated tests
  instead of `#[allow]`-ing).
- 2 new unit tests on top of the 7 existing ones (288/288 total, +2).
- Clippy + fmt + cargo test all green.

**Decisions:**
- `quiet` parameter lives on `gc_with_known_detailed` rather than a separate
  builder/struct. It's a single internal call site (`do_run`) plus the tests;
  a builder would add ceremony with no payoff.
- Config-load / qBittorrent fetch failures in JSON mode print `{ "error":
  "<msg>" }` to stdout so a JSON consumer never sees a half-rendered table —
  mirrors how `tql doctor --json` keeps a single document on stdout.
- `kept` entries are included in the JSON `entries[]` (not just orphans).
  Operators correlating GC output with their torrent list benefit from a
  complete view; the cost is `O(sidecars)` which is the same as the plain
  human path anyway.

**Notes for future sessions:**
- The `quiet=true` in tests also suppresses the `"would unlink ..."` lines
  that previously went to stdout in dry-run mode. The 7 pre-existing tests
  never asserted on those lines, so this is silent. If you ever want to
  assert on them, capture stdout in a child process or thread `quiet=false`
  through.

## 2026-05-11 — Leg 36: `tql sidecar repair`

**What got built:**
- New `src/cmd/sidecar_repair.rs` (~340 LoC incl. 7 tests).
  - Reuses `sidecar_verify::scan` + `Issue` enum as the discovery pass.
  - `MissingResolved` → `link_to_site`; `InodeMismatch` → `unlink_site` then
    `link_to_site`; `MissingContent` / `ReadError` → unrepairable skip.
  - Stop boundary for unlink is `<library_root>/<category>/` (same convention
    as `post_process` / `sidecar_gc`).
  - `cfg.linking.prefer` honored via the same `map_strategy` helper pattern.
- `cmd/mod.rs` + `main.rs` wire it in as `SidecarAction::Repair`.
- Tests cover: relink missing target (asserts inode equality with seed),
  replace inode-mismatched target, dry-run touches nothing but emits
  `would relink` + `planned=1`, missing content_path→skip→exit-1, empty
  metadata dir is `scanned=0` ok, clean world `repaired=0` ok, JSON shape
  has summary + actions.
- 295/295 tests green (+7 from 288); clippy + fmt clean.

**Decisions:**
- Reused `sidecar_verify::scan` instead of re-implementing the issue logic.
  Keeps `verify` and `repair` symmetric — if verify gains a new issue kind
  later, repair gets a free upgrade (or a compile error in the `match`,
  which is the better failure mode).
- Exit 1 on `skipped` *and* `failed`. A `MissingContent` skip is still an
  unhealthy state operators should know about; folding it into exit-0 would
  silently mask data loss. Dry-run with planned actions still exits 0
  because the operator explicitly asked "would this work?" — they'll re-run
  without `--dry-run` to actually act.
- For inode mismatch, we `unlink_site` (which prunes empty parents up to the
  category boundary) and then `link_to_site` (which re-creates them via
  `create_dir_all`). Slight wasted work compared to "remove then rename in
  place," but it lets us reuse the canonical primitives unchanged. The
  pruning never reaches the category boundary because the category dir is
  the stop_at — safe.
- Repair only handles single-file inode mismatches today (verify only checks
  inode for `is_directory == false`). Directory torrents get the existence
  check from verify and an existence-based relink from repair — a full
  recursive structural check is left to a future sub-leg if it ever comes
  up in practice.

**Notes for future sessions:**
- `dry_run` actions print `would relink` / `would replace`. The "skip" line
  intentionally omits the `would ` prefix because skipping happens
  identically with or without `--dry-run`.
- If verify ever adds a new issue variant, the `match` in `plan_then_apply`
  will fail to compile — that's intentional. Add the new arm with an
  appropriate `ActionKind` (or a `Skip` if unrepairable).

## 2026-05-11 — Session 37 (Leg 37)

**State at start:** 36 legs done; 295/295 tests green. All open follow-ups
parked. Picked the explicit Leg-34 caveat — directory torrent verify only
checked existence, not recursive inode equality — as the next concrete
gap to close.

**Done:**
- Added `dir_tree_drifted(content, site)` + `collect_tree` walker to
  `sidecar_verify`. Walks `content_path` once into a BTreeMap of
  `relative_path → (dev, ino)` (plus a set for symlinks), then probes
  each site under those relative paths.
- `check_sidecar` directory branch now calls the walker when both content
  and site exist; any drift collapses to one site-level `InodeMismatch`.
- 3 new tests: hardlinked subtree passes; missing child fails;
  independent-copy child fails. Existing empty-tree test still passes.

**Decisions:**
- Site-level `InodeMismatch` rather than a new `DirChildDrift` variant —
  repair already handles InodeMismatch on directory sites via Replace
  (unlink + relink full tree), so adding a variant would just be cosmetic.
  The verify report loses per-child granularity; if operators need it,
  a future `--verbose` flag can re-emit the per-file diff.
- One-directional invariant: extras under the site are tolerated.
  Rationale: linking.rs writes a tree under a sibling-temp and renames;
  it never deletes user-dropped extras at the site (a README the user
  added, a Plex `.plexmatch`, etc.). Flagging those as drift would
  break in-the-wild setups.
- Symlinks: existence-checked only. linking.rs reproduces them as
  symlinks rather than hardlinking the target's inode, so requiring
  `(dev, ino)` equality there would always fire false positives.

**Notes for future sessions:**
- The walker is recursive and unbounded — for pathological directory
  torrents this could blow the stack. Worth converting to an explicit
  worklist if anyone reports it; not worth it preemptively.
- `dir_tree_drifted` returns `bool` (drift / no drift). If we ever want
  per-child diagnostics, change the signature to return `Vec<ChildDiff>`
  and thread that into the Issue payload.

## Leg 38 — Wire `tracing` + `tracing-subscriber` (2026-05-11)

**What landed:**
- `Cargo.toml` adds `tracing = "0.1"` and `tracing-subscriber = "0.3"`
  with the `env-filter`, `fmt`, `json`, and `registry` features.
- New `src/logging.rs` with `init()` that installs a global subscriber:
  stderr fmt layer (human-readable, target hidden) + optional JSONL file
  layer when `$TQL_LOG_FILE` is set. Filter resolution uses `$TQL_LOG`
  via `EnvFilter`, defaulting to `info`. `try_init` so the call is
  idempotent / cheap from tests.
- `main.rs` calls `logging::init()` immediately after `Cli::parse()` so
  every subcommand inherits it.
- Startup `tracing::info!` events added to `cmd::api::run` (`%addr,
  "listening"`) and `cmd::mcp::run` (stdio "ready" + HTTP "listening").
  Pre-existing `eprintln!` lines kept alongside — many tests assert on
  stderr capture, and the marginal value of churning them isn't worth a
  test rewrite this session.
- 6 new tests in `logging::tests` (filter default + env override, log
  file env unset/empty/set, parent-dir auto-create, idempotent init).

**Decisions:**
- Env-only knobs (`TQL_LOG`, `TQL_LOG_FILE`) instead of a `[logging]`
  TOML block. DESIGN.md §6 only specifies the dependency, not the
  surface. Adding TOML keys is easy later; locking them in now would be
  premature.
- File layer uses `Mutex<File>` for the writer so we don't bring in
  `tracing-appender` for a single-line append-only file. The append
  semantics are kernel-level (`O_APPEND`); the mutex just serializes
  the formatter's per-event multi-write sequence.
- One transient flake observed during the full-suite run
  (`cmd::reload::tests::stale_pid_file_is_treated_as_no_server`); it
  passes in isolation and on retry. Unrelated to logging; the test does
  PID-file dance and may race with itself when other tests touch the
  filesystem under heavy load.

**Notes for future sessions:**
- When/if a real user wants TOML-controlled logging, the `Config`
  extension is one struct: `pub logging: Logging { file: Option<PathBuf>,
  level: Option<String> }`. `init()` already has the shape; we'd just
  thread an `Option<&Config>` into it.
- The `eprintln!`→`tracing` migration is still open. It's a tedious
  per-call review (each one needs a level decision and the matching
  tests rewritten to use `tracing_subscriber::fmt::TestWriter` or a
  capture layer). Worth doing if/when an operator complains about
  JSONL output missing details that only stderr has today.

## 2026-05-11 — Session 39

**State at start:** Leg 38 (tracing wiring) landed cleanly. PLAN's "future
work" listed crates.io publish, rmcp swap, SSE, KVM CI — all bigger than a
focused session. The `--json` polish trio for `doctor`/`sidecar gc` had a
gap: `notify-flush` still emitted only human lines + per-event JSONL on
dry-run.

**Done — Leg 39 (`tql notify-flush --json`):**
- Added `Args::json` and `render_json(&Outcome) -> String`.
- `run` routes config-load + tokio-runtime errors through the JSON shape
  on stdout when `--json` is set (preserves the existing exit-code 2 for
  bootstrap failures).
- 4 new tests; full suite 308/308 green; clippy + fmt clean.

**Decisions:**
- Reused the existing `Outcome` enum verbatim — no new public type. The
  flat `{outcome, ...}` shape is parser-friendly without dragging in a
  Serde-tagged enum (which would have forced renaming the variants).
- Pretty-printed (vs. compact) JSON to match `sidecar gc --json` /
  `doctor --json` — operators already see multi-line output from those.
- Dry-run JSON moves from per-event JSONL on stdout (one line per event)
  to a single document with `pending: [...]`. The old format only fired
  on `--dry-run` so no other tool can have been parsing it; flipping it
  under `--json` keeps the single-document invariant.

**Notes for future sessions:**
- A symmetric `--json` for `tql reconcile` is the next obvious operational
  polish — it still prints `tql reconcile: {N total, ...}` to stderr and
  exits 1 on aborts. Same pattern as this leg.

## 2026-05-11 — Session 40

**State at start:** Leg 39 (`notify-flush --json`) landed. The session-39
notes flagged `tql reconcile --json` as the next obvious `--json` polish,
but `tql test --json` is closer to the doctor/gc/notify-flush template —
no qBittorrent mock surface to design, just a fixture-summary shape — and
just as useful for CI integrations that already invoke `tql test` in
pipelines.

**Done — Leg 40 (`tql test --json`):**
- Added `Args::json` to `cmd::test`.
- New `render_json(...)` emits `{trackers_loaded, load_failures, summary,
  failures, exit_code}`. `failures[].kind` is one of
  `io|parse|input|classify|mismatch` (mapped via `failure_kind_str`).
- Config/registry/unknown-tracker errors emit `{error: "<msg>"}` on
  stdout in JSON mode (mirrors session 39's bootstrap-error handling).
- 4 new tests (json pass, json fail, render_json shape, render_json with
  load_failures). Full suite 312/312 green; clippy + fmt clean.

**Decisions:**
- Did not reshape `run_all` to expose per-fixture passes — the human
  output never listed them either, and emitting just failures (with the
  passed/failed counts in `summary`) keeps the JSON small. A future leg
  can layer in a `verbose` mode if needed.
- `failure_kind_str` is a private free function, not a `Display` impl on
  `FixtureFailureKind`, because the `Display` of `FixtureFailure` already
  embeds the kind in the human form — adding a second `Display` would be
  ambiguous.
- Did NOT pick `tql reconcile --json` (the session-39 suggestion). It's
  the right next leg, but it needs more thought about the per-torrent
  shape (planned vs aborted vs warnings) — splitting it off keeps this
  session short.

**Notes for future sessions:**
- `tql reconcile --json` is still the next obvious polish: `Outcome`
  variants per torrent + a global summary. Pattern is identical to
  Legs 35/39/40.

## 2026-05-11 — Leg 41 (`tql reconcile --json`)

**State at start:** Leg 40 done (315 tests would be too high — checked, it
was actually 312). The previous session flagged reconcile-json as the next
obvious polish.

**Done:**
- `cmd/reconcile.rs`: added `Args::json`, introduced `Report { dry_run,
  summary, entries }`, factored per-outcome printing into
  `Report::print_human`, added `render_json`.
- Tests updated to consume `do_run(...).summary` (was bare `Summary`).
- 3 new tests for JSON path; full suite 315/315 green; clippy + fmt clean.

**Decisions:**
- Kept the `Skip` outcome distinct in the JSON (`status: "skipped"`) rather
  than folding into `aborted` or hiding it — operators benefit from seeing
  exactly which torrents were skipped and why (no category, etc.).
- Config/transport errors emit `{error: "<msg>"}` on stdout in JSON mode so
  a single parser can consume both happy and unhappy paths, matching the
  shape already used by legs 35/39/40.

**Notes for future sessions:**
- Five subcommands now expose `--json`: doctor, sidecar gc/list/verify,
  notify-flush, test, reconcile. Remaining candidates: `tql sidecar show`
  (already prints JSON, but a stable wrapper might still be useful) and
  `tql post-process` (probably not — it's expected to be quiet by §7).

## 2026-05-11 — Leg 42 (`tql link {add,remove} --json`)

**State at start:** Leg 41 done. Surveyed remaining JSON-family candidates;
`tql link add` / `tql link remove` were unflagged but easy and useful — both
are operator-facing mutators that scripts will want to wrap.

**Done:**
- `cmd/link.rs`: factored `run` → `do_run` returning a `Report` enum
  (`Ok { warnings } | Error(msg)`). Added `render_json(op, hash, path,
  report)` and a `json: bool` parameter to `run`. The wrappers
  `cmd::link_add::Args` / `cmd::link_remove::Args` each grow `--json`.
- Existing end-to-end tests updated to pass `json = false`.
- 2 new tests for the JSON shape; full suite 317/317 green; clippy + fmt
  clean (one fmt re-run picked up a needless line-break in the new test).

**Decisions:**
- Did not include `adds`/`removes` lists in the JSON. The non-dry-run
  `post_process::Outcome::Ok` arm doesn't surface them (only `Planned`
  does), and `link {add,remove}` always runs the live path. Keeping the
  payload `{op, hash, path, status, warnings?, error?}` mirrors what the
  command actually knows after the fact.
- Kept the Aborted reason as a single `error` string rather than a
  structured `{code, message}`: the upstream variants are heterogeneous
  enough that a code enum would be premature.

**Notes for future sessions:**
- JSON family now covers: doctor, sidecar gc/list/verify, notify-flush,
  test, reconcile, link add/remove. The only mutators still without
  `--json` are `post-process` (intentionally quiet per §7) and
  `sidecar repair` — repair was already done in Leg 36. So the family
  is essentially complete.
- Reload test `cmd::reload::tests::stale_pid_file_is_treated_as_no_server`
  flaked once during the full suite run and then passed in isolation;
  worth a closer look if it recurs (likely a PID-reuse race when many
  tests run in parallel).

## 2026-05-11 — Session 43

**State at start:** Leg 42 landed; Leg 38 (tracing wiring) committed but its
PLAN entry was never marked DONE. Two HTTP transports (`tql api`,
`tql mcp --http`) had no per-request tracing — just the startup `info!` line.

**Done:**
- Marked Leg 38 DONE in PLAN.md (the code was complete; the marker was the
  only thing missing).
- Leg 43: added `src/cmd/http_trace.rs` with a hand-rolled axum middleware
  that emits `tracing::info!(method, path, status, elapsed_ms,
  "http_request")` per request, suppressing `/health`. Wired into
  `api::router` and `mcp::http_router` via `axum::middleware::from_fn`.

**Decisions:**
- One shared middleware module under `cmd/` rather than a separate top-level
  module; both consumers are command-layer routers and the helper is tiny.
- Suppress `/health` rather than down-leveling it to `debug`: most operators
  run with `TQL_LOG=info`, and a steady stream of probe lines is noise.
- No `tower-http` dep. `TraceLayer` would give us spans for free but pulls
  in extra surface for a 30-line helper; reassess if we want request IDs or
  spans-per-request later.

**Outcome:**
- 319/319 tests green (+2 in `cmd::http_trace::tests`).
- `cargo fmt --check` + `cargo clippy --bin tql --tests -- -D warnings` clean.

## 2026-05-11 — Leg 44: `tql cli --dry-run --json`

**Goal:** small operational polish on `tql cli`. The post-add ack is already
JSON, but `--dry-run` emitted a human-readable preview block. Added a `--json`
flag that swaps the preview for a single JSON document so the dry-run path is
also pipeline-friendly.

**Done:**
- `cmd/cli.rs`: `Args` and `dispatch` grow a `json: bool`. New
  `render_preview_json(tracker, source, kind, input, output) -> String` emits
  `{dry_run: true, tracker, source: {kind, value}, input, link_tags,
  info_tags, warnings}` (pretty). When `--dry-run` is absent the flag is a
  no-op (the post-add ack is unconditionally JSON already, by design).
- New test `render_preview_json_shape`.

**Outcome:**
- 320/320 tests green (+1 in `cmd::cli::tests`; +2 if you count Leg 43's
  delta since reporting).
- `cargo fmt` + `cargo clippy --bin tql --tests -- -D warnings` clean.

## 2026-05-11 — Session: Leg 45

**Done:**
- Added `--hash <HASH>` filter to `tql sidecar verify` and `tql sidecar repair`.
  Case-insensitive match against `info_hash_v1`; no-match → stderr error + exit 1.
- Threaded `hash_filter: Option<&str>` through `verify()` / `repair()`.
- Migrated existing test call sites to pass `None`.
- 4 new tests (2 per command: happy case-insensitive + no-match-is-error).

**Outcome:**
- 324/324 tests green (+4).
- `cargo fmt` + `cargo clippy --bin tql --tests -- -D warnings` clean.

**Notes:**
- Considered making no-match a soft no-op (exit 0). Picked exit-1 because the
  flag is operator-typed; a silent zero-scan would mask typos. JSON mode still
  surfaces the error to stderr — could mirror it as `{error:"…"}` to stdout in
  a future polish, but no caller is parsing the hash-filter outcome separately
  yet.

## 2026-05-11 — Session: Leg 46

**Goal:** add a `--category <CAT>` filter to `tql sidecar list` so operators
can scope the listing to one tracker bucket. Parallels Leg 45's `--hash`
filter on `verify` / `repair`.

**Done:**
- `cmd/sidecar_list.rs::Args` grows `category: Option<String>`.
- `list()` gains a `category_filter: Option<&str>` parameter and calls
  `summaries.retain(|s| s.category.to_lowercase() == needle)` after `collect()`.
- Match is case-insensitive (sites in DESIGN are lowercase domain names, but
  user-typed input shouldn't have to be).
- 2 new tests (`list_category_filter_restricts_results_case_insensitively`,
  `list_category_filter_with_no_match_returns_empty`).
- Updated 4 existing test call sites to pass `None`.

**Outcome:**
- 326/326 tests green (+2).
- `cargo fmt` + `cargo clippy --bin tql --all-targets -- -D warnings` clean.

**Notes:**
- Diverged from Leg 45's no-match-is-error policy. For `list`, an empty result
  is a normal observation ("are there any in this category?"); for action
  commands (`verify`/`repair`) it's a likely typo. Exit 0 with empty array/no
  lines fits `list_empty_when_metadata_dir_missing`'s precedent.

## 2026-05-11 — Leg 47: `--category` filter on `tql sidecar verify` / `tql sidecar repair`

**Goal:** Round out the filter trio (`--hash` on verify/repair from Leg 45, `--category` on list from Leg 46). Add `--category` to verify/repair so a tracker-scoped audit/heal can run without grep on human output (which can't drive per-sidecar repair planning anyway).

**Plan:**
- Extend `cmd::sidecar_verify::Entry` with `category: Option<String>` — `None` on `read_error` (no parseable sidecar to extract a category from). `verify_one()` populates `Some(sc.category)` on success.
- Add `--category <CAT>` to both `verify::Args` and `repair::Args`; thread `category_filter: Option<&str>` through `verify()` and `repair()`. Filter applies after the `--hash` filter (intersection).
- Match is case-insensitive (`to_lowercase()` on both sides), matching Leg 46.
- Apply Leg 45's no-match-is-error policy (these are action commands, unlike `list`): empty post-filter set → `eprintln!` + `Err(1)`.

**Implementation:**
- `Entry::category` plumbed through `scan()` → both consumers. Repair re-reads the sidecar per-entry already, so this is purely a filter-time optimization, not a behavior change.
- Existing test call sites: 13 in verify, 9 in repair, all updated to pass `None` for the new param.
- 4 new tests using `mk_file_sidecar_cat` helpers that override the default `demo.org` category.

**Outcome:**
- 330/330 tests green (+4) under `--test-threads=1`. (Default parallelism occasionally trips a pre-existing flake in `cmd::reload::tests::stale_pid_file_is_treated_as_no_server` — same `lock()` mutex pattern as adjacent tests; serial run is clean. Not my code; flagging for a future session.)
- `cargo clippy --bin tql --all-targets -- -D warnings` clean.

**Notes:**
- Filters compose: `--hash X --category Y` requires both. Order in `verify()`/`repair()` is hash first then category, but the operation is intersection so order is observable only in the diagnostic on empty (it reports the first filter that emptied things, which is fine).

## 2026-05-11 — Leg 48: `tql reload --json`

**Goal:** close the JSON family. `reload` was the last operational command
without `--json`.

**Done:**
- `cmd/reload.rs` factored: new `do_run(&Args) -> Outcome` separates the work
  from rendering. `Outcome::Ok { validated: Option<usize>, load_failures,
  signaled: Vec<Signal>, errors }` + `Outcome::Error(String)` for fatal
  config/trackers-root loads.
- `Args` grows `--json`. `render_json` emits `{outcome, validated,
  load_failures, signaled: [{role, pid}], errors, no_server}` or
  `{outcome:"error", message}` for the fatal case. When `errors` is non-empty
  the outcome tag flips to `"error"` (matching exit code 1).
- 4 new tests covering JSON shapes; existing 3 reload tests updated to pass
  `json: false`.

**Decisions:**
- Did not split into a separate `Outcome::Error` for the partial-errors case
  (e.g. SIGHUP delivery failed but config loaded). Kept those inside
  `Outcome::Ok::errors` and let the renderer flip the outcome tag. Simpler
  shape and keeps `validated`/`signaled` visible alongside the error list.
- `signaled` is emitted as `[{role, pid}]` (objects, not a map) so that the
  ordering (`api` then `mcp`) is observable for callers that care.
- `no_server` is a derived boolean; cheaper for downstream tooling than
  reconstructing it from `signaled.is_empty() && errors.is_empty()`.

**Outcome:**
- 334/334 tests green (+4) under `--test-threads=1`.
- `cargo clippy --bin tql --all-targets -- -D warnings` clean.

---

## 2026-05-11 — Leg 49: `tql completions <shell>`

**Decisions:**
- Picked shell completions as the next leg because the JSON-output family is
  exhausted (Leg 48 closed it) and DESIGN.md does not mandate a richer next
  step. Completions are a small, contained ergonomics win that exercises only
  the existing `clap` tree — no business logic changes, no risk to the
  operational commands.
- Generator runs against `crate::Cli::command()`, which forced `Cli` from
  private to `pub`. Acceptable: it's the natural top-level type and only the
  `cmd::completions` test code consumes it programmatically.
- Test surface is intentionally cheap: three smoke tests that confirm the
  three most-used shells produce non-empty output mentioning the binary name
  and (for bash) a known subcommand. `clap_complete` itself is well-tested
  upstream; we just need to confirm wiring.
- `render` helper is `#[cfg(test)]` to avoid a dead-code warning in release
  builds (the production path streams straight to stdout).

**Outcome:**
- 337/337 tests green (+3).
- `tql completions bash|zsh|fish|elvish|powershell` emits a usable script.
- Adds `clap_complete = "4"`.

---

## Leg 50 — `--hash` / `--category` filters on `tql sidecar gc` (2026-05-11)

**Context:**
- Legs 45 and 47 added `--hash` / `--category` filters to `sidecar verify` and
  `sidecar repair`; Leg 46 added `--category` to `sidecar list`. `sidecar gc`
  was the remaining sidecar subcommand with no scoping flags.

**Approach:**
- Args grew `hash: Option<String>` and `category: Option<String>`. Plumbed
  through `do_run` → `gc_with_known_detailed`.
- Hash filter retains by lowercase compare. Category filter peeks at each
  sidecar via `sidecar::read`; unreadable sidecars are dropped from the
  filtered set (the upstream `EntryStatus::ReadError` path would have surfaced
  them anyway, but only inside the un-filtered scope).
- No-match → `summary.errors += 1`, eprintln, return. The existing
  `run` wrapper turns any non-zero errors into exit 1.
- qBittorrent `known` set is *not* narrowed by the filters. Keeping the full
  live set means we still classify the filtered sidecars as kept vs orphan
  correctly; we just process fewer of them.

**Decisions / surprises:**
- I considered making `gc_with_known_detailed` return `Result<Report, String>`
  to model "no match" cleanly, but the existing call sites all rely on the
  `errors`-counted exit path, and a unified error channel was a bigger
  refactor than this leg deserved. The errors-counter route is consistent
  with the `read_dir` failure handling already in the function.
- The category filter pays the cost of a `sidecar::read` per candidate even
  for sidecars that end up kept. Acceptable: gc runs are operator-triggered,
  not on the hot path.

**Outcome:**
- 341/341 tests green (+4).
- `tql sidecar gc --hash <H>` and `tql sidecar gc --category <C>` work,
  optionally combined with `--dry-run` and `--json`.
- No new deps.

## Leg 51 — `tql config show` subcommand (2026-05-11)

**Plan recap:** Add a small operational subcommand that prints the effective
loaded config (after TOML + `$TQL_*` env overlay) as pretty JSON, plus the
path it was loaded from. `--path-only` shortcuts to just the path. No new
deps; reuses `serde_json` and the existing `config::load`.

**Implementation notes:**
- New module `src/cmd/config_show.rs`, modeled after `cmd::sidecar_show`
  (`run` wraps an inner `show(path, cfg, path_only, &mut out)` so tests can
  capture stdout without going through the on-disk loader).
- Wired in `main.rs` under a `Config { action: ConfigAction }` enum with a
  single `Show` variant today; leaves room for a future `Config Validate`
  without a breaking flag rename.
- Initial test fixture used a non-existent `username_env` field on
  `QBittorrent`; quick `grep` confirmed the real field is `username` (plain
  string) — fixed the test before running. Caught in seconds; no commits
  needed to undo.

**Decisions / surprises:**
- Considered emitting TOML to match the on-disk format, but figment's env
  overlay can introduce types that don't TOML-round-trip cleanly (and the
  rest of the JSON family in the codebase makes JSON the obvious default).
  Skipped a `--toml` flag — easy to add later if anyone asks.
- Considered showing the *search order* with checkmarks per candidate
  (`~/.config/tql/config.toml` ✓, `/etc/tql/config.toml` ✗, …). Deferred —
  one resolved path is enough for the 90% case, and search-order debugging
  is the rare case that `--config <PATH>` already covers.

**Outcome:**
- 344/344 tests green (+3).
- `tql config show` and `tql config show --path-only` work.
- No new deps.

## Leg 52 — `tql config init` (2026-05-11)

**What:** New `Config Init` subcommand that scaffolds a starter `config.toml`
with placeholder values matching DESIGN.md §11. `Config Show` answers
"what's loaded?", but until a config exists there's nothing to load — new
operators were copying snippets out of DESIGN.md by hand. The natural
follow-on to Leg 51.

**Shape:** `src/cmd/config_init.rs` plus a sibling
`config_init_template.toml` brought in via `include_str!`. Args: `--output
PATH` to override target, `--force` to overwrite, `--stdout` to skip the
filesystem (useful for `… | sudo tee /etc/tql/config.toml`). Default
target: `$XDG_CONFIG_HOME/tql/config.toml` (or `$HOME/.config/tql/...`).
Refuses to overwrite without `--force`; creates parent dirs on demand.

**Template safety:** every secret is referenced by env-var name only
(`password_env`, `bot_token_env`, `token_env`, `api_key_env`,
`cookie_env`, `auth_header_env`). The test
`template_contains_no_plaintext_secrets` asserts none of the obvious
plaintext field-name patterns (`password = "…"`, `api_key = "…"`,
`token = "…"`, `secret = "…"`) show up in the template, so a future
edit can't accidentally inline a footgun.

**Round-trip guarantee:** `template_parses_as_loadable_config` writes the
template into a tempdir and feeds it through `config::load`, so any drift
between the `Config` struct and the starter template surfaces as a test
failure rather than a confusing first-run error for the operator.

**Wiring:** `ConfigAction::Init(cmd::config_init::Args)` slots in next to
`Show`. `--stdout` uses clap's `conflicts_with_all = ["output", "force"]`
so the CLI errors clearly if combined.

**Decisions / surprises:**
- Considered emitting the template by `serde_json::to_string(&Config::default())`
  → `toml::to_string`. Rejected: `Config::paths` has no Default (required
  fields), and the round-trip wouldn't preserve the section ordering or
  inline comments that make the file readable. Hand-written template +
  load-round-trip test is the right trade-off.
- Considered a `--stdout` *plus* `--output -` convention. Picked just
  `--stdout` to keep the path/stdin sentinel discussion off the table.

**Outcome:**
- 349/349 tests green (+5).
- `tql config init --stdout`, `tql config init --output PATH`, and the
  default-XDG flow all work.
- No new deps (uses `include_str!`).

## Leg 53 — `tql config validate` (2026-05-11)

**Why:** Leg 52's future-work list called out `Config::Validate` as a
deeper static-check counterpart to `doctor`. `doctor` is great when the
target machine has reachable qBittorrent + a populated trackers root, but
operators rolling config out to a fresh host want a purely offline
verifier that flags structural bugs (typos in env-var names, an http
URL with the wrong scheme, an `mcp.transport = "http"` with no
`api_key_env`, a `[trackers.<name>]` block that references a manifest
that isn't on disk) without trying to log in to anything.

**Shape:** `cmd::config_validate::run(Args)` mirrors `doctor::run`'s
shape but skips `--probe`/login/fixture execution. Reuses
`doctor::{Check, Status, render_json}` so `--json` output is
byte-compatible with `doctor --json` (same `checks`/`summary`/`exit_code`
keys). The five check buckets are:

- `check_paths` — absolute + exists + is_dir for the three roots.
- `check_urls` — `reqwest::Url::parse` + scheme allowlist.
- `check_env_vars` — `std::env::var` lookup, status message names the
  *env-var key only*, never the value. Belt-and-braces test
  (`env_check_never_leaks_value`) sets a recognizable plaintext into
  the env and asserts it never appears in human output.
- `check_mcp_http` — cross-section invariant from DESIGN §11.
- `check_trackers_static` — runs `scripting::registry::load_dir` (which
  is static; no fixtures), then set-diffs the manifest names against
  `cfg.trackers` keys.

**Decisions / surprises:**
- Considered re-running `scripting::fixtures::run_all` to mirror what
  `doctor` does. Rejected: fixtures execute Rhai scripts, so by
  definition they aren't a static check — that's `doctor`'s job, and
  duplicating it would force `validate` to take the same wall-clock hit
  it was meant to avoid.
- Considered emitting a different JSON shape ("structural-only"). Kept
  `doctor`'s shape so JSON consumers (CI gates) don't need a second
  parser. The check *names* differentiate the two.
- `Registry` exposes `.names()`, not `.keys()` — caught at compile
  time, fixed by switching to the public iterator.

**Outcome:**
- 357/357 tests green (+8) when run in isolation. One pre-existing
  flake (`cmd::reload::tests::stale_pid_file_is_treated_as_no_server`)
  is a PID-collision race unrelated to this leg — passes in isolation.
- Clippy clean under `-D warnings`.
- No new deps; reuses `reqwest::Url` which was already pulled in.

## Leg 54 — NixOS VM check against a real qbittorrent-nox (2026-05-11)

**Why:** PROMPT.md instructs that once feature work is done the next
focus is "integration with NixOS/HM modules" and "leverage NixOS checks
to perform end-to-end testing with a real qbittorrent+testing tracker".
The existing `nixos-module` check only touched the bundled HTTP server;
nothing in CI ever actually spoke qBittorrent's WebUI. This leg adds
that coverage.

**Shape:** `nix/test-qbittorrent.nix` is a standalone
`pkgs.testers.runNixOSTest` invocation that wires:
- a `qbt` system user with `/var/lib/qbt` as profile dir,
- a pre-seeded `/etc/qbt/qBittorrent.conf` copied via `systemd.tmpfiles`
  into `/var/lib/qbt/qBittorrent/config/qBittorrent.conf` (the path
  qBittorrent v5 actually reads when given `--profile=DIR`),
- a systemd unit running `qbittorrent-nox --profile=/var/lib/qbt
  --webui-port=8082`,
- `services.tql` with `api.enable = true`, `qbittorrent` settings
  pointing at `127.0.0.1:8082`, and an `environmentFile` that exports
  `TQL_QBIT_PASSWORD=adminadmin`.

The test script waits for both units, sanity-checks that the seeded
credentials log into qBittorrent directly (`/api/v2/auth/login`
returns `Ok.`), then invokes `tql doctor --config <rendered-config>
--json` under `systemd-run --pipe --wait
--property=EnvironmentFile=…` so the password env var is loaded for
the ad-hoc command. The rendered config path is recovered at runtime
via `systemctl show -p Environment tql-api.service`, since the
module-generated TOML lives at a non-deterministic `/nix/store/…`
path. The script parses the JSON output and asserts
`qbittorrent.login` and `qbittorrent.version` are both `status: ok`.

**Pre-seeded credentials:** the canonical PBKDF2-SHA512 string from
the qBittorrent wiki for the `adminadmin` password is used. The
WebUI conf also turns off CSRF/HostHeader checks so that intra-VM
localhost calls don't bounce — these are test-only knobs, not
recommended for real deployments.

**Decisions / surprises:**
- First VM run failed because `qbittorrent-nox` ignored the seeded
  conf and printed a temporary password on stdout. Root cause: I
  placed the file under `/var/lib/qbt/.config/qBittorrent/`
  (the default Linux config dir), but `--profile=DIR` overrides
  that and reads `DIR/qBittorrent/config/qBittorrent.conf`. Fixed
  by relocating the tmpfile.
- Considered using `pkgs.lib.fileContents` + a checked-in
  `qBittorrent.conf` fixture. Kept the conf inline so the leg's
  intent (seed exactly these credentials, nothing else) stays
  legible in one file.
- Considered a fully end-to-end test that adds a torrent and
  verifies the sidecar tree. Deferred — it requires a fake tracker
  + a fixture torrent + peers, which is a substantially bigger leg
  than "prove tql can talk to qbittorrent at all". Logged as future
  work in PLAN.md.

**Outcome:**
- `nix build .#checks.x86_64-linux.nixos-qbittorrent` succeeds; the
  test script runs in ~22s once the system image is built.
- All 10 doctor checks report (9 ok, 1 expected warn for the
  `--probe`-only `probe` check); `qbittorrent.login` reports
  `http://127.0.0.1:8082 as admin` ok, `qbittorrent.version`
  reports `v5.1.4` ok.
- `flake.nix` exposes `checks.<system>.nixos-qbittorrent` alongside
  the existing `nixos-module`. No Rust source changes; no new deps.
