# CLAUDE.md — Working with this repo

## Project

`tql` — single Rust binary, multi-mode (mcp, api, cli, post-process, reconcile, etc.).
See `DESIGN.md` for the full design contract.

## Toolchain

This is NixOS. No global Rust toolchain. Use:

```sh
nix shell nixpkgs#cargo nixpkgs#rustc nixpkgs#gcc -c <command>
```

For convenience, prefer wrapping cargo invocations inline:

```sh
nix shell nixpkgs#cargo nixpkgs#rustc nixpkgs#gcc -c cargo build
nix shell nixpkgs#cargo nixpkgs#rustc nixpkgs#gcc -c cargo test --bin tql
nix shell nixpkgs#cargo nixpkgs#rustc nixpkgs#gcc -c cargo check
```

Note: this crate is binary-only (no `lib` target). `cargo test --lib` errors
with "no library targets found". Use `cargo test --bin tql` to run unit tests
inside the binary.

A Nix flake with a devshell may be added later; until then, the `nix shell` form works fine and the store paths are cached.

## Layout

Single Cargo project at the repo root. Source under `src/`. See DESIGN.md §6 for the planned module tree.

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
