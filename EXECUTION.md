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
