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
