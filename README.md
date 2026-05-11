# tql — Tracker-Qualified Layout

`tql` is a single-binary Rust tool that organizes qBittorrent downloads into a
ghq-style, tracker-faithful directory tree. Per-tracker classification logic
runs as sandboxed [Rhai] scripts, and the same logic is exposed through three
transports — MCP, REST, and CLI — plus a post-process hook and a periodic
reconcile job.

For the full design contract, see [DESIGN.md](DESIGN.md). For the
implementation roadmap and per-leg history, see [PLAN.md](PLAN.md) and
[EXECUTION.md](EXECUTION.md).

## At a glance

- **Category** = canonical tracker domain (e.g. `myanonamouse.net`).
- **`link:<rel-path>` tags** = hardlink sites under
  `<library_root>/<category>/`.
- **Sidecar** at `<library_root>/.metadata/<info-hash-v1>.json` is the source
  of truth for what `tql` applied last.
- **Scripts** in `<trackers_root>/<name>/` (manifest + Rhai) turn structured
  per-tracker input into `{category, link_tags, info_tags}`.

The post-processor never parses torrent names and never knows tracker-specific
rules — all of that lives in the scripts. See DESIGN.md §1, §5, §10.

## Build

NixOS, no global Rust toolchain. From the repo root:

```sh
nix build                 # produces ./result/bin/tql
nix develop --command cargo build --release
nix develop --command cargo test --bin tql
```

`nix build` consumes the flake's `packages.default` (a `buildRustPackage`
derivation) and is the recommended path for installs. The devshell
(`nix develop`) gives you `cargo`, `rustc`, `rustfmt`, `clippy`, `gcc`, and
`pkg-config` for day-to-day iteration. The crate is binary-only — use
`cargo test --bin tql`, not `--lib`.

## Subcommands

| Command                | Purpose                                                          | DESIGN ref |
| ---------------------- | ---------------------------------------------------------------- | ---------- |
| `tql mcp`              | MCP server (`--stdio` or `--http <addr>`), one tool per tracker. | §7, §14    |
| `tql api`              | Axum REST server, one endpoint per tracker.                      | §7, §13    |
| `tql cli <tracker> …`  | Per-tracker CLI; flags built dynamically from the manifest.      | §7, §12    |
| `tql post-process`     | qBittorrent "torrent finished" hook (always exits 0).            | §7, §8     |
| `tql reconcile`        | Periodic safety-net; bounded parallelism.                        | §7         |
| `tql link add/remove`  | Manual `link:` tag manipulation.                                 | §7         |
| `tql sidecar show`     | Dump the sidecar JSON for a hash.                                | §7         |
| `tql test [tracker]`   | Run all fixtures (`fixtures/*.toml`) under a tracker.            | §7         |
| `tql notify-flush`     | Drain the notification spool to Telegram/etc.                    | §15        |
| `tql doctor [--probe]` | Validate config, paths, trackers, qBittorrent, optional probes.  | §7, §16    |
| `tql reload`           | SIGHUP a running `tql api` / `tql mcp` to re-read trackers.      | §7, §16    |

## Configuration

Search order (first hit wins):

1. `$TQL_CONFIG`
2. `$XDG_CONFIG_HOME/tql/config.toml`
3. `/etc/tql/config.toml`

Env overrides use `TQL_<SECTION>__<KEY>` (double underscore = section split).
The full schema is documented in DESIGN.md §11.

## Layout

Trackers live in `<trackers_root>/<name>/`:

```
<trackers_root>/<name>/
  manifest.toml         # input schema + canonical category
  classify.rhai         # pure: input → {category, link_tags, info_tags}
  fixtures/*.toml       # input + expected_output pairs for `tql test`
```

The shipped `trackers/example/` is exercised by `tql test`.

## License

MIT OR Apache-2.0.

[Rhai]: https://rhai.rs/
