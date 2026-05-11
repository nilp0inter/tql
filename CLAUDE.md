# CLAUDE.md — Working with this repo

## Project

`tql` — single Rust binary, multi-mode (mcp, api, cli, post-process, reconcile, etc.).
See `DESIGN.md` for the full design contract.

## Toolchain

This is NixOS. No global Rust toolchain. A flake-based devshell is provided:

```sh
nix develop --command cargo build
nix develop --command cargo test --bin tql
nix develop --command cargo check
```

The devshell ships `cargo`, `rustc`, `rustfmt`, `clippy`, `gcc` (linker), and
`pkg-config`. Run `nix develop` with no args to drop into an interactive shell.

Legacy fallback (still works, no flake.lock needed):

```sh
nix shell nixpkgs#cargo nixpkgs#rustc nixpkgs#gcc -c <command>
```

Note: this crate is binary-only (no `lib` target). `cargo test --lib` errors
with "no library targets found". Use `cargo test --bin tql` to run unit tests
inside the binary.

## Layout

Single Cargo project at the repo root. Source under `src/`. See DESIGN.md §6 for the planned module tree.

`trackers/` at the repo root holds example tracker bundles (manifest +
script + fixtures). Point a config's `paths.trackers_root` here and run
`tql test --config <cfg>` to exercise the fixture runner end-to-end.

## Workflow

Each session:
1. Read `PLAN.md`, pick next pending task (or write a new leg if empty).
2. Do exactly that task; keep sessions short.
3. Update `PLAN.md`, `EXECUTION.md`, and (if structure/process changed) this file.
4. Commit and push.

## Conventions

- Commits use prefixes: `feat:`, `fix:`, `doc:`, `chore:`, `refactor:`, `test:`.
- Stub modules are fine — fill them out in later legs as PLAN dictates.
- Don't invent features beyond what DESIGN.md spells out.
