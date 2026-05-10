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

### Leg 4 — Sidecar read/write (sidecar.rs)

Atomic write under `flock`, schema_version=1, round-trip tests. Adds `fs2`.

### Leg 5 — Linking primitives (linking.rs)

`link(2)` / reflink / atomic rename. Single-file and directory cases.
Tested against tmpfs in integration tests.

### Leg 6 — qBittorrent WebUI client (qbit/)

Login, addTorrent, getTorrents. Mock server in tests.

### Leg 7 — Tracker manifest parsing (scripting/manifest.rs)

Parse `manifest.toml`; build the per-tracker Input schema. No Rhai yet.

### Leg 8 — Rhai sandbox + classify execution (scripting/)

Engine setup per DESIGN.md §12 sandboxing rules. Run `classify(input)`,
collect `ClassifyOutput`, validate against §5 producer rules.

### Leg 9 — Tracker registry + `tql test` (fixtures runner)

Discover all trackers, run all fixtures.

### Leg 10 — `tql post-process`

The qBittorrent hook. Wires sidecar + linking + validation together.

### Leg 11 — `tql reconcile`

The safety net.

### Leg 12 — Transports: CLI subcommands per tracker

Dynamic clap from manifests.

### Leg 13 — Transports: REST (`tql api`)

Axum server. Adds `axum`, `tokio`, `reqwest`, optionally `utoipa`.

### Leg 14 — Transports: MCP (`tql mcp`)

`rmcp` integration.

### Leg 15 — Notifications & media-server refresh

Telegram, Plex/Jellyfin.

### Leg 16 — `tql doctor` full checks; `tql reload`; polish

Final integration, end-to-end docs.

(Each leg may spawn sub-legs as detail emerges. Reorder freely if priorities
shift; record reordering rationale in EXECUTION.md.)
