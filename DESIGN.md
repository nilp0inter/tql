# `tql` — Tracker-Qualified Layout

A multi-mode Rust binary for organizing qBittorrent downloads in a ghq-style,
tracker-faithful directory layout. Per-tracker logic is implemented as
sandboxed Rhai scripts; the same logic is exposed as an MCP server, a REST
API, and a CLI subcommand tree.

This document is the design contract for the implementation. It is written
for a coding assistant with no prior context on the project. Read it
end-to-end before making structural changes.

---

## 1. Concept

The layout on disk follows the model of `ghq`: every torrent's library path
begins with the canonical tracker domain (mirroring `github.com/`,
`bitbucket.org/`, etc.). Below that, the path is whatever the tracker itself
organizes torrents by — categories, sections, format/media facets, author or
artist — captured faithfully from the tracker's own page.

Most trackers file a single torrent under multiple categories simultaneously,
so the layout supports **multi-homing**: one physical file, many hardlinked
locations.

The qBittorrent state stores everything needed to reproduce the layout:

- **Category** = the canonical tracker domain (single-valued).
  - Drives ATM seeding-tree partitioning.
  - Drives per-tracker seeding-policy rules.
  - Identifies which tracker's rules produced the tags.
- **Tags prefixed `link:`** = relative-to-category library paths
  (multi-valued).
  - One tag per intended hardlink site.
  - Path is relative to `<library_root>/<category>/`.
  - Cannot escape the category directory.
- **Other tags** = freeform, informational, ignored by the post-processor.

The post-processor reads category and `link:` tags and produces a hardlink
forest. It does not parse torrent names, consult per-tracker schemas, or
know anything about specific trackers. The producer of the tags is the only
place tracker-specific knowledge lives.

### Tools = add operations, not classifiers

There is no generic "add to qBittorrent" exposed to the LLM, to REST clients,
or to the CLI. Every entry point is **per-tracker**. The LLM (or any
caller) chooses *which tracker tool to invoke*; the tool decides the
category, the link tags, and everything else. A misbehaving caller cannot
produce a torrent in the wrong category or with arbitrary tags, because no
endpoint exists that takes a category and tags as input.

The per-tracker logic — the function from `(structured fields)` to
`{category, link_tags, info_tags}` — is implemented as a sandboxed Rhai
script. Scripts are colocated with a manifest declaring the input schema, so
the same definition is exposed through three transports without
hand-duplication.

The shared Rust core handles:

- Fetching the .torrent (from a file, URL, or magnet) — never done by the
  script.
- Calling the qBittorrent WebUI to add the torrent with the
  script-determined category and tags.
- Validating script outputs against the tag and category contracts (§5).

The script handles:

- Mapping tracker-specific fields to the canonical category (a hard-coded
  string per tracker).
- Producing the list of `link:` tags from the input.
- Producing optional informational tags.
- Returning warnings the operator should see.

### Surfaces

The same Rust binary, selected by subcommand, runs as:

1. **`tql mcp`** — MCP server, one tool per tracker.
2. **`tql api`** — REST server, one endpoint per tracker.
3. **`tql cli <tracker> ...`** — CLI subcommand, one per tracker.
4. **`tql post-process`** — qBittorrent's "torrent finished" hook.
5. **`tql reconcile`** — periodic safety net that aligns the filesystem with
   current qBittorrent tags.
6. Utility commands: `tql link {add,remove}`, `tql sidecar show`, `tql test`,
   `tql doctor`, `tql reload`.

There is no daemon shared between transports. Each of `mcp`, `api`,
`post-process`, and `reconcile` is its own process.

---

## 2. Non-goals

These are deliberately out of scope. Do not invent them.

- **No generic add-to-qBittorrent endpoint.** Every transport's add path is
  per-tracker.
- **No internal classification DSL beyond Rhai.** Rhai *is* the DSL.
- **No file renaming for seeding torrents.** The seeding copy in
  `<seed_root>/<category>/` is byte-identical to what qBittorrent
  downloaded.
- **No deletion of seeding files.** qBittorrent owns `<seed_root>`.
- **No quality, duplicate, or version judgments.** That is the *arrs' job.
- **No tracker-URL parsing at apply time.** The category is the
  authoritative tracker name. `%T` is not consulted by the post-processor.
- **No symlinks for primary link sites.** Hardlink or fail. Reflink is
  acceptable on supporting filesystems.
- **No content-based classification.** No `guessit`, no ffprobe, no MIME
  sniffing.
- **No web UI.**

---

## 3. Architecture overview

```
       LLM agent              REST client              human at terminal
       (with CDP)              (autobrr,                (ad-hoc adds)
           │                    *arr, etc)                    │
           │ MCP                     │ HTTP                   │ exec
           ▼                         ▼                        ▼
       ┌────────────┐         ┌────────────┐         ┌────────────┐
       │ tql mcp    │         │ tql api    │         │ tql cli    │
       │ (stdio/    │         │ (HTTP)     │         │ <tracker>  │
       │  HTTP)     │         │            │         │            │
       └─────┬──────┘         └─────┬──────┘         └─────┬──────┘
             │                      │                      │
             └──────────────┬───────┴──────────────────────┘
                            │ deserialized into per-tracker Input
                            ▼
              ┌──────────────────────────────────┐
              │  scripting host (Rhai engine)    │
              │                                  │
              │  trackers/<name>/classify.rhai   │
              │     pure: Input -> ClassifyOut   │
              └────────────────┬─────────────────┘
                               │ {category, link_tags, info_tags}
                               ▼
              ┌──────────────────────────────────┐
              │  shared Rust core (in-process)   │
              │  • fetch .torrent (file/URL/     │
              │    magnet)                       │
              │  • POST to qBittorrent WebUI     │
              │    add endpoint                  │
              └────────────────┬─────────────────┘
                               │ qBittorrent state holds
                               │ category + link: tags
                               ▼
                       ┌─────────────────┐
                       │  qBittorrent    │
                       └────────┬────────┘
                                │ on completion
                                ▼
              ┌──────────────────────────────────┐
              │  tql post-process                │
              │  • reads %L %G %N %F %I          │
              │  • applies link: tags            │
              │  • writes sidecar                │
              │  • notifies, refreshes Plex      │
              └──────────────────────────────────┘

              ┌──────────────────────────────────┐
              │  tql reconcile (cron/timer)      │
              │  • diff sidecar vs current tags  │
              │  • add/remove link sites         │
              └──────────────────────────────────┘
```

Three properties of this layout are load-bearing:

1. **The script is pure.** It cannot fetch URLs, open files, spawn processes,
   or read time. The Rhai sandbox is configured to forbid these. All it
   does is transform structured input to structured output.
2. **The script never sees the .torrent.** The .torrent source (file path,
   URL, or magnet) is part of the transport input but is passed directly to
   the Rust core, bypassing the script. A buggy script cannot mis-direct a
   download.
3. **The transports are thin.** Each transport (MCP, REST, CLI) is a
   serialization layer over the same `(input, torrent_source) → result`
   pipeline. They share validation, sandboxing, qBittorrent client code,
   logging, error handling.

---

## 4. Layout on disk

```
<seed_root>/                         # owned by qBittorrent ATM
  myanonamouse.net/
    Some.Book.Title/
  redacted.ch/
    ...

<library_root>/                      # owned by tql, hardlinks only
  myanonamouse.net/
    Computer/Internet/
      Hamza Farooq/
        Build an LLM Application from Scratch/   ← hardlink of seed
    Education/Textbook/
      Hamza Farooq/
        Build an LLM Application from Scratch/   ← same inodes
    _authors/
      Hamza Farooq/
        Build an LLM Application from Scratch/
  redacted.ch/
    FLAC/CD/
      Radiohead/
        In Rainbows/
  _public/
    ...

<library_root>/.metadata/            # sidecar database (per torrent)
  <info-hash-v1>.json

<trackers_root>/                     # tracker scripts and manifests
  myanonamouse/
    manifest.toml
    classify.rhai
    fixtures/
      basic.toml
      multi-category.toml
  redacted/
    manifest.toml
    classify.rhai
    fixtures/
      ...
```

Constraints, all enforced in code:

- `<seed_root>` and `<library_root>` **must be on the same filesystem.** The
  doctor command verifies `st_dev` equality.
- `<library_root>/.metadata/` is reserved.
- `<trackers_root>` defaults to `/etc/tql/trackers/` or
  `$XDG_CONFIG_HOME/tql/trackers/`; overridable in config.

---

## 5. The `link:` tag contract

The runtime contract between the scripts (which produce tags) and the
post-processor (which consumes them).

### Syntax

A link tag is a qBittorrent tag of the form:

```
link:<relative-path>
```

where `<relative-path>` is forward-slash-separated and has at least one
non-empty component. The tag is relative to `<library_root>/<category>/`;
it never includes the canonical tracker name.

### Resolution

For a torrent with category `C`, name `N`, and a link tag `link:R`, the
hardlink site is:

```
<library_root> / C / R / sanitize(N)
```

Multi-file torrents recreate their content directory at that location with
every file hardlinked. Single-file torrents become a regular file at that
path. `sanitize(N)` is defined in §10.

### Validation rules (post-processor)

A link tag is **rejected** if:

1. The torrent has no category.
2. The relative path is empty.
3. The relative path is absolute (starts with `/`).
4. Any path component is `.`, `..`, or empty.
5. The first path component is `.metadata`.
6. The relative path contains a NUL byte.
7. The resolved path escapes `<library_root>/<category>/`.
8. The resolved path exceeds `PATH_MAX`.

Validation is per-tag. A torrent with one valid and one invalid tag is
partially applied; the invalid one is reported.

### Producer rules (scripts)

The scripting host enforces these *before* a script's output reaches the
qBittorrent add layer:

- Every link tag begins with literal `link:`.
- No link tag's path begins with the canonical category.
- Forward slashes only.
- At least one link tag is returned.
- Each path component passes sanitization (§10).

A script that returns a malformed result causes the *transport call* to
fail (returning an error to the LLM/REST client/CLI), and nothing is added
to qBittorrent.

### Tag length

Soft caps: 256 bytes per `link:` tag, 16 link tags per torrent. Exceeding
either is logged WARN and proceeds; this is a sign of a script bug to be
investigated.

---

## 6. The Rust project shape

Single Cargo project, single binary. Subcommands select mode.

```
tql/
  Cargo.toml
  src/
    main.rs                   # CLI dispatch; clap top-level
    config.rs
    paths.rs                  # sanitization, validation, resolution
    sidecar.rs
    linking.rs                # link(2) / reflink / atomic-rename
    qbit/
      mod.rs                  # WebUI client (login, addTorrent, getTorrents)
      types.rs
    notify/
      mod.rs                  # Notification trait
      telegram.rs
      apprise.rs              # behind feature flag
    media/
      mod.rs                  # MediaServer trait
      plex.rs
      jellyfin.rs
    scripting/
      mod.rs                  # Rhai engine setup, sandboxing, host fns
      manifest.rs             # parse manifest.toml; build Input schema
      registry.rs             # discover & load all trackers/<name>/...
      types.rs                # ClassifyOutput, host ↔ script marshaling
      sandbox.rs              # disable file/net/process modules
    transports/
      mod.rs                  # shared "classify then add" pipeline
      mcp.rs                  # MCP server (stdio + HTTP)
      api.rs                  # REST server (axum)
      cli.rs                  # dynamic clap subcommands per tracker
    cmd/
      post_process.rs         # tql post-process
      reconcile.rs            # tql reconcile
      mcp.rs                  # tql mcp
      api.rs                  # tql api
      cli.rs                  # tql cli <tracker> ...
      link_add.rs             # tql link add
      link_remove.rs          # tql link remove
      sidecar_show.rs
      doctor.rs               # tql doctor
      test.rs                 # tql test (run tracker fixtures)
      reload.rs               # tql reload (re-read trackers/)
  tests/
    integration/
  trackers/                   # shipped example trackers; user trackers
                              # in /etc/tql/trackers or XDG config
    myanonamouse/
      manifest.toml
      classify.rhai
      fixtures/
```

### Dependency notes

- **CLI**: `clap` v4 with derive *and* dynamic `Command` building (for the
  per-tracker `cli <tracker> ...` subcommands).
- **Config**: `serde` + `figment` (TOML + env overrides).
- **Logging**: `tracing` + `tracing-subscriber`. JSONL to file; human to
  stderr.
- **Async**: `tokio`.
- **HTTP server**: `axum`.
- **HTTP client**: `reqwest`.
- **MCP**: `rmcp` (the official Rust MCP SDK).
- **Scripting**: `rhai`. Use `Engine::new_raw()` to start without standard
  modules and re-enable only the safe ones (string ops, math, arrays,
  maps); explicitly *do not* register `eval`, file, network, or process
  modules.
- **Schema generation**: `schemars` for the JSON Schema we hand to MCP/REST,
  generated from the manifest.
- **Filesystem**: stdlib + `nix` for `link(2)`/`renameat2(2)`/`flock(2)`;
  `reflink-copy` for reflink.
- **Sidecar locking**: `fs2`.
- **TOML**: `toml`.
- **OpenAPI**: `utoipa` (optional) for the REST `/openapi.json`.

Avoid heavy frameworks. The binary should compile in under a minute on a
modest machine.

---

## 7. Subcommands

### `tql mcp`

Runs the MCP server.

- `--stdio` (default): JSON-RPC over stdio.
- `--http <addr>`: JSON-RPC over HTTP for shared multi-client setups.

Tools are registered dynamically from `<trackers_root>`. Each tracker
becomes one tool, named `tracker.<name>.add` (e.g.,
`tracker.myanonamouse.add`). The tool's input JSON Schema comes from the
manifest; the tool's description comes from the manifest's `description`
field; field-level descriptions come from per-field `description` keys in
the manifest.

The tool's behavior:

1. Validate input against schema.
2. Run `classify.rhai` with input → `ClassifyOutput`.
3. Validate `ClassifyOutput` against §5 producer rules.
4. Fetch the .torrent from the input's `source` field (file/URL/magnet).
5. Call qBittorrent's `/api/v2/torrents/add` with the `.torrent`, the
   determined category, and the determined tags.
6. Return a small acknowledgment: `{ok: true, info_hash, category,
   link_tags, info_tags, qbit_response_summary}`.

Any failure returns an MCP tool error; nothing is added.

### `tql api`

Runs the REST server (axum).

Endpoints:

- `POST /trackers/<name>/add` — body is the same JSON the MCP tool accepts,
  with one extra field `source` for the .torrent (see §8). Returns the
  same shape as MCP on success.
- `GET /trackers` — list of registered trackers and their canonical
  categories.
- `GET /trackers/<name>/schema` — JSON Schema of the tracker's input.
- `GET /openapi.json` — full OpenAPI 3 doc, generated from manifests at
  startup.
- `GET /health` — `{ok: true}`.

Auth: a single API key in a request header, configured via env var. No
per-route ACL in v1.

### `tql cli <tracker> [flags...] <source>`

A clap subcommand per tracker, generated dynamically at startup from
manifests. Each tracker exposes its input fields as long flags
(`--categories`, `--author`, etc.); the .torrent source is the positional
argument.

Examples:

```
tql cli myanonamouse \
  --url 'https://www.myanonamouse.net/t/123456' \
  --categories 'Computer/Internet' \
  --categories 'Education/Textbook' \
  --author 'Hamza Farooq' \
  --label 'LLM' --label 'Education' \
  /tmp/build-an-llm.torrent

tql cli redacted --format FLAC --media CD --artist 'Radiohead' \
                 --release-type Album \
                 'magnet:?xt=urn:btih:...'
```

`tql cli` (no tracker) lists all available trackers and their canonical
categories, with one-line descriptions.

`tql cli <tracker> --help` prints flags derived from the manifest, with
descriptions and enum values.

### `tql post-process`

The qBittorrent hook. See §8 for argv. Behavior:

1. Parse arguments (long flags only).
2. Acquire `flock` on `<library_root>/.metadata/<hash>.lock` (max wait 30 s).
3. Sanity-check category (non-empty, no slashes, sanitizable).
4. Parse `link:` tags from `--tags`. If zero valid, quarantine.
5. Compute desired link sites; diff against existing sidecar.
6. Apply diff (create new, remove stale).
7. Write sidecar atomically.
8. Enqueue notification.
9. Trigger media-server refresh (best-effort).

Always exits 0. Failures are recorded in the sidecar and the notification
spool.

### `tql reconcile`

Walks all qBittorrent torrents and runs the same diff/apply logic as
post-process. Modes:

- (default): apply changes.
- `--dry-run`: print diff, change nothing.
- `--torrent <hash>`: limit to one.
- `--category <name>`: limit to one tracker.

Concurrency: per-hash `flock`; bounded global parallelism (config).

### `tql link add <hash> <path>` / `tql link remove <hash> <path>`

Manual add/remove of a `link:` tag. Updates qBittorrent tags and triggers a
single-torrent reconcile. Used for ad-hoc fixes when an upstream system
didn't.

### `tql sidecar show <hash>`

Print sidecar JSON.

### `tql test [tracker]`

Run all fixtures (or just one tracker's). Each fixture is a TOML file with
`input` and `expected_output`; the runner loads the script, calls it with
`input`, asserts equality. CI-friendly. Exits non-zero on first failure.

### `tql reload`

Re-read `<trackers_root>` and rebuild the registry. Sends a signal to a
running `mcp` or `api` server (PID file under `/run/tql/` or
`$XDG_RUNTIME_DIR/tql/`) which triggers an in-process reload. If no server
is running, this is a no-op + warning. The default mode is static loading
at startup; reload is opt-in via this command.

### `tql doctor`

Validates the installation:

- Config parses, required fields set.
- `<seed_root>` and `<library_root>` exist, same filesystem.
- `<library_root>/.metadata/` exists and is writable.
- `<trackers_root>` exists; every tracker module loads without errors.
- For every tracker: manifest parses, script parses, fixtures pass.
- qBittorrent WebUI reachable (auth check, `app/preferences`).
- With `--probe`: send a test notification, hit each media server.

Exit non-zero on any failure.

---

## 8. The .torrent source field

The transport input has two parts:

1. **Tracker fields** (script input) — the structured input the script
   consumes. Defined per-tracker in the manifest.
2. **Source** (Rust core input) — *where the .torrent comes from*. The
   script does not see this.

`source` is a tagged union with three variants:

```json
// File path on the host
{ "kind": "file", "path": "/tmp/foo.torrent" }

// HTTP(S) URL — Rust core fetches with credentials per config
{ "kind": "url", "url": "https://tracker/torrents.php?id=...&authkey=..." }

// Magnet link
{ "kind": "magnet", "uri": "magnet:?xt=urn:btih:..." }
```

For URL fetches, the Rust core consults a per-tracker credential table in
config (cookies or auth headers) to authenticate to the tracker site. The
script never has access to credentials.

CLI maps the source to a positional argument — file paths, URLs, and
magnets are detected by prefix. REST and MCP take it in the request body.

The post-processor invocation from qBittorrent (§ below) is unrelated to
this; the post-processor reads from the qBittorrent state, not from a
.torrent.

### qBittorrent hook command line (post-process)

```
/usr/local/bin/tql post-process \
  --hash "%I" \
  --name "%N" \
  --category "%L" \
  --tags "%G" \
  --content-path "%F" \
  --save-path "%D" \
  --size "%Z"
```

Long flags only. Empty `%G` is harmless because tags are passed as a single
quoted argument.

---

## 9. Linking semantics

### Algorithm

For each desired link site `T` of a torrent with content path `S`:

1. If `T` already exists:
   - If `S` is a single file and `stat(T).st_ino == stat(S).st_ino`:
     idempotent success.
   - If `S` is a directory and `T` mirrors it via hardlinks:
     idempotent success.
   - Otherwise: refuse and report. Operator resolves.
2. Build at sibling temp `T.tmp.<rand>` on the same filesystem.
3. Single-file torrent: `link(2)` `S` → temp. `EXDEV` is fatal config
   error.
4. Multi-file: recreate directory tree, `link(2)` each regular file.
   Reproduce symlinks as symlinks.
5. `rename(2)` temp → `T`.
6. On any failure, remove the partial temp tree.

Reflink is used in place of hardlink when the filesystem supports it and
config says so. From the sidecar's perspective they are equivalent.

### Never

- `link(2)` across `st_dev` with a copy fallback. Fail loudly.
- Modify or replace `S`.
- Overwrite a `T` that isn't already a correct hardlink.

### Empty-parent pruning

When removing a stale link site, walk upward removing newly-empty
directories *until* hitting `<library_root>/<category>/`. Stop there.
Never delete the category directory.

---

## 10. Path sanitization

Used by the post-processor and by the script-host's output validator.

For one component:

1. NFC normalize.
2. Trim leading/trailing whitespace.
3. Replace NUL and `/` with `_`.
4. If `windows_compat` (default true):
   - Replace `<>:"|?*\` with `_`.
   - Replace trailing `.` and trailing space with `_`.
   - Prepend `_` to Windows reserved names (`CON`, `PRN`, etc.).
5. Truncate to 200 bytes (UTF-8 safe).
6. If empty after the above, replace with `_`.

For a multi-component path: split on `/`, sanitize each, rejoin. Reject if
> 32 components.

Locale-independent. Case-preserving.

---

## 11. Configuration

```toml
[paths]
seed_root = "/data/torrents"
library_root = "/data/library"
trackers_root = "/etc/tql/trackers"

[linking]
prefer = "hardlink"            # or "reflink" or "reflink_or_hardlink"
windows_compat = true

[qbittorrent]
url = "http://127.0.0.1:8080"
username = "admin"
password_env = "QBIT_PASSWORD"

[notify]
default = ["telegram"]

[notify.telegram]
bot_token_env = "TELEGRAM_BOT_TOKEN"
chat_id = "-1001234567890"
parse_mode = "HTML"

[media.plex]
url = "http://127.0.0.1:32400"
token_env = "PLEX_TOKEN"
section_ids = [1, 2, 3]

[media.jellyfin]
url = "http://127.0.0.1:8096"
api_key_env = "JELLYFIN_KEY"

[reconcile]
parallelism = 4
adopt_orphans = true
remove_stale = true

[mcp]
transport = "stdio"            # or "http"
http_addr = "127.0.0.1:7373"

[api]
addr = "127.0.0.1:7374"
api_key_env = "TQL_API_KEY"

[scripting]
reload_on_change = false       # opt-in dynamic reload
max_script_runtime_ms = 200    # Rhai operation budget
max_script_memory_mb = 16

# Per-tracker credentials for URL .torrent fetches (optional)
[trackers.myanonamouse]
cookie_env = "MAM_COOKIE"      # value of mam_id cookie

[trackers.redacted]
auth_header_env = "RED_AUTHKEY"
```

Secrets always by env var name.

---

## 12. Tracker module authoring

A tracker is a directory under `<trackers_root>` containing a manifest, a
script, and fixtures.

### Layout

```
trackers/myanonamouse/
  manifest.toml
  classify.rhai
  notify.rhai        # optional, overrides global FieldManipulation (§15.1)
  notify.hbs         # optional, overrides global Handlebars template (§15.1)
  fixtures/
    basic.toml
    multi-category.toml
    no-author.toml
```

### `manifest.toml`

The manifest is the single source of truth for the tracker's metadata and
input schema. Everything the LLM, the REST client, and the CLI need is
derived from it.

```toml
# trackers/myanonamouse/manifest.toml

name = "myanonamouse"
canonical_category = "myanonamouse.net"
version = "1.0.0"
description = """
Add a torrent from MyAnonamouse to qBittorrent.

MyAnonamouse files torrents under one main category and frequently under
one or more cross-reference categories (visible in the "File Info" section
as multiple links). All categories should be supplied; one link site is
generated per category.

The "Author" field on the torrent page becomes part of the path. If the
torrent has no author, omit it; the path will use the category alone.
"""

# URL pattern that matches torrent pages on this tracker. Used as a hint
# to the LLM and for URL→tracker auto-routing in transports.
url_pattern = '^https?://(www\.)?myanonamouse\.net/t/\d+'

# Each input field is declared here. The order is the order of CLI flags
# in --help.
[[input]]
name = "url"
type = "string"
required = true
description = "Tracker URL of the torrent page (for logging only)."

[[input]]
name = "categories"
type = "array<string>"
required = true
min_items = 1
description = """
All categories the torrent is filed under. Each category is its own entry;
subcategories use forward slashes (e.g., "Computer/Internet"). Order
doesn't matter.
"""
cli_flag = "--categories"           # CLI: repeat the flag per item

[[input]]
name = "author"
type = "string"
required = false
description = """
Author name as displayed on the torrent page. For multiple authors, join
with " & ".
"""

[[input]]
name = "labels"
type = "array<string>"
required = false
default = []
description = "Free-form labels from the 'Tags and Labels' line."
cli_flag = "--label"

[[input]]
name = "language"
type = "string"
required = false
description = "Language as displayed on the page. Used as info tag only."
```

Manifest types: `string`, `int`, `bool`, `array<T>`, `enum<...>`, `map<string,
string>`. The host generates the JSON Schema, clap arguments, and OpenAPI
schema from this declaration.

### `classify.rhai`

A pure function from the validated input map to a result map. The script
receives `input` as a Rhai object map and returns an object map with three
fields: `link_tags` (array of strings), `info_tags` (array of strings),
`warnings` (array of strings).

The script does **not** return the category; the host injects
`canonical_category` from the manifest. This makes it impossible for the
script to mis-set the category.

```rhai
// trackers/myanonamouse/classify.rhai
//
// Pure function: validated input -> {link_tags, info_tags, warnings}.
// No I/O, no time, no randomness. Sandboxed.

fn classify(input) {
    let link_tags = [];
    let info_tags = [];
    let warnings = [];

    // Build one link tag per category, with author appended if present.
    for cat in input.categories {
        let path = if input.author != () && input.author != "" {
            cat + "/" + input.author
        } else {
            cat
        };
        link_tags.push("link:" + path);
    }

    // Add an _authors index when an author is known.
    if input.author != () && input.author != "" {
        link_tags.push("link:_authors/" + input.author);
    }

    // Labels become info tags, prefixed.
    if input.labels != () {
        for label in input.labels {
            info_tags.push("label:" + label);
        }
    }

    if input.language != () && input.language != "" {
        info_tags.push("language:" + input.language);
    }

    if link_tags.is_empty() {
        warnings.push("no link tags produced");
    }

    #{ link_tags: link_tags, info_tags: info_tags, warnings: warnings }
}
```

### Fixture format

```toml
# trackers/myanonamouse/fixtures/basic.toml

[input]
url = "https://www.myanonamouse.net/t/123456"
categories = ["Computer/Internet"]
author = "Hamza Farooq"
labels = ["LLM"]
language = "English"

[expected_output]
link_tags = [
  "link:Computer/Internet/Hamza Farooq",
  "link:_authors/Hamza Farooq",
]
info_tags = [
  "label:LLM",
  "language:English",
]
warnings = []
```

### Script sandboxing rules (enforced by the host)

The Rhai engine is configured with:

- `Engine::new_raw()` (no standard modules loaded by default).
- Re-enable: `BasicArrayPackage`, `BasicStringPackage`, `BasicMapPackage`,
  `BasicMathPackage`, `LogicPackage`.
- Do **not** register: any I/O, any time, any random, any
  `eval`/`Engine::eval_*` recursion.
- `set_max_operations(N)` from config (default 200,000).
- `set_max_string_size(...)`, `set_max_array_size(...)`, `set_max_map_size(...)`.
- `set_max_call_levels(...)` for stack-depth bound.

A script that exceeds operation or memory bounds returns a host error; the
transport call fails; nothing is added.

### Host-provided helpers

A small set of Rhai utility functions the host registers, all pure:

- `sanitize(component: string) -> string` — applies §10 to one component.
- `lower(s: string) -> string`, `trim(s: string) -> string`,
  `replace(s, from, to)` — delegate to Rhai's standard string ops.
- `slug(s: string) -> string` — opinionated slugger for filename-safety
  (NFKD-fold + replace non-`[A-Za-z0-9._-]` with `_`). Optional sugar.

No tracker-fetching, no URL-parsing, no JSON. If a script needs a URL
parsed, it parses with string ops or the manifest declares a more
structured input type.

---

## 13. Transports — one tracker, three surfaces

The same tracker module is exposed through MCP, REST, and CLI. The host
generates each surface from the manifest and the script.

### Shared pipeline

```
input (raw, transport-specific)
  └─→ deserialize (per-transport)
        └─→ validate against JSON Schema (from manifest)
              └─→ run classify.rhai with validated input
                    └─→ validate output against §5 contract
                          └─→ fetch .torrent from `source` (file/URL/magnet)
                                └─→ POST to qBittorrent /api/v2/torrents/add
                                      └─→ format response per transport
```

Steps 3–7 are identical across transports. The first two and the last vary.

### MCP

- Tool name: `tracker.<name>.add`.
- Input schema: derived from manifest; appended is a `source` field with
  `kind = file|url|magnet` and matching value (mirroring the REST shape).
- Tool description: from manifest's `description` field.
- Field descriptions: from manifest's per-field `description`.

### REST

- `POST /trackers/<name>/add` with body:

  ```json
  {
    "input": { /* per-tracker fields */ },
    "source": { "kind": "url", "url": "..." }
  }
  ```

- 200 with the same acknowledgment shape on success.
- 4xx on input validation failure with details.
- 5xx on qBittorrent or fetch failure.

### CLI

- `tql cli <tracker> --field value ... <source>`.
- Source is a positional argument; type detected by prefix:
  - `magnet:` → magnet
  - `http://` / `https://` → url
  - otherwise → file (path on host).
- Flags generated from manifest. Array fields take repeated flags by
  default; if the manifest sets `cli_separator = ","` the field accepts a
  comma-separated value instead.

### Schema/manifest changes

Adding a tracker is creating a directory. Reloading is `tql reload` (or
restart). Editing a manifest's input schema is a breaking change for
existing callers and should bump the manifest's `version`; the host logs a
warning at load time when a manifest has no version or an older one than
last seen.

---

## 14. Sidecar format

Per-torrent JSON at `<library_root>/.metadata/<info_hash_v1>.json`:

```json
{
  "schema_version": 1,
  "info_hash_v1": "abc123...",
  "info_hash_v2": null,
  "name": "Build an LLM Application from Scratch",
  "category": "myanonamouse.net",
  "content_path": "/data/torrents/myanonamouse.net/Build.../",
  "is_directory": true,
  "size_bytes": 4760000,
  "link_sites": [
    {
      "relative_path": "Computer/Internet/Hamza Farooq",
      "resolved_path": "/library/myanonamouse.net/Computer/Internet/Hamza Farooq/Build.../",
      "created_at": "2026-05-10T12:34:56Z",
      "origin": "post_process"
    }
  ],
  "last_applied_tags": ["link:Computer/Internet/Hamza Farooq"],
  "last_applied_at": "2026-05-10T12:34:56Z",
  "warnings": []
}
```

Origin values: `post_process`, `reconcile`, `manual`, `cross_seed` (reserved,
unused in v1).

Writes are atomic (write-temp-then-rename) under exclusive `flock`. Reads
take shared `flock`. A missing sidecar means "fresh"; a malformed one
quarantines the torrent.

---

## 15. Notifications, media-server refresh, concurrency, errors,
       logging, testing

These match the previous design and are summarized here for completeness.

- **Notifications**: file-backed JSONL spool drained by `tql notify-flush`
  (separate systemd-timer). Default Telegram, optional apprise behind a
  feature flag. Debounce window 5 s, max batch 10. Message content is
  produced by a two-stage pipeline (see §15.1).

### 15.1 Notification rendering pipeline

Each spooled `Event` is rendered to a backend-targeted string by a
two-stage pipeline:

```
EventFields -> FieldManipulation (Rhai) -> Template (Handlebars) -> NotificationText
```

**Stage 1 — FieldManipulation** is a pure Rhai function:

```rhai
fn shape(fields, escape) {
    // fields: object map mirroring Event (name, category, info_hash_v1,
    //         link_sites_added, link_sites_removed, warnings, ts, ...)
    // escape: fn(string) -> string suitable for the target backend syntax
    //         (HTML for Telegram-HTML, MarkdownV2 for Telegram-MD, identity
    //         for plain). The same script works for every backend because
    //         escaping is dependency-injected.
    fields.title = escape(fields.name) + " [" + escape(fields.category) + "]";
    fields.has_added = fields.link_sites_added.len() > 0;
    fields.added_block = fields.link_sites_added
        .map(|s| "• " + escape(s))
        .join("\n");
    fields
}
```

Input is one event at a time (not a batch). The function returns a new
fields map; the original keys may be retained, replaced, or augmented
with derived values. Rhai's op-count bound (`scripting.max_script_runtime_ms`)
applies.

**Stage 2 — Template** is a Handlebars template rendered against the
post-manipulation fields map. Handlebars was chosen deliberately: its
logic is intentionally limited to truthy sections (`{{#if}}`,
`{{#each}}`, `{{else}}`) and registered helpers — no inline comparisons,
arithmetic, or filters. Anything beyond branch-on-a-value belongs
upstream in Rhai. This keeps the boundary clear:

- **Rhai**: synthesize, derive, escape, decide booleans.
- **Handlebars**: lay out already-shaped strings; branch on flags Rhai
  precomputed.

Built-in helpers are limited to the Handlebars defaults; tql does not
register custom helpers (any helper would just move logic the script
should own into the template).

### 15.2 Override resolution

Both stages are overridable globally and per-tracker. Resolution order
for a given event with `category = "<tracker>"`:

1. `trackers/<tracker>/notify.rhai` and `trackers/<tracker>/notify.hbs`
   if present in the tracker bundle.
2. Global overrides referenced from `[notify]` config
   (`script_path`, `template_path`).
3. Embedded defaults shipped in the binary (`include_str!`), which
   reproduce today's hardcoded Telegram format.

Each pair is resolved independently — a tracker bundle may ship only a
template (reusing the global/default script), only a script (reusing
the global/default template), or both. The default script is the
identity transform plus a small set of conventional derived fields
(`title`, `added_block`, `removed_block`, `has_warnings`) that the
default template consumes.

### 15.3 Batching

The drainer renders each event independently through the pipeline, then
concatenates the rendered strings into a single backend message
(Telegram message, apprise notification body). Batch-level summaries
("3 new books from MAM") are out of scope for v1 — if needed later they
can be added as a second template applied to the array of rendered
events, without changing the per-event pipeline above.
- **Media refresh**: per-link-site partial scan against Plex
  (`/library/sections/<id>/refresh?path=...`) and Jellyfin
  (`/Library/Media/Updated`). Best-effort, 5 s timeout, no retry.
- **Concurrency**: per-info-hash `flock`; reconcile uses bounded
  parallelism (config); MCP/REST use `tokio` task per request; scripts
  run synchronously inside a request and are bounded by Rhai op count.
- **Errors**: single crate-wide enum; post-processor always exits 0;
  reconcile reports aggregate failure count; doctor exits non-zero on
  failure; transports surface tool errors with stripped credentials.
- **Logging**: JSONL to file, human to stderr in CLI modes. Required
  fields: `ts`, `level`, `mode`, `event`. Forbidden in logs: passkeys,
  tracker passkey URLs, qBittorrent credentials, env-var values.
- **Testing**: `tql test` runs all fixtures; unit tests in Rust for
  sanitization, link tag validation, sidecar round-trip, linking
  primitives; integration tests against a mock qBittorrent (axum) and
  tmpfs roots; property tests for sanitization idempotency and link
  tag containment.

---

## 16. Operational notes

- The post-processor is invoked per completion. It must complete in under
  60 seconds.
- The reconciler is the safety net. Run it on a 5–15 minute timer.
- The `mcp` and `api` servers are long-running, supervised by systemd with
  restart-on-failure.
- `tql doctor` after every config or manifest change.
- Backups: the sidecar directory and the trackers directory. The library
  tree itself is reproducible from qBittorrent tags + sidecar.

---

## 17. Open questions

- **Hot reload of trackers.** The doc specifies opt-in via `tql reload`.
  Live reload via filesystem watch is left as a v2.
- **Multiple library roots.** Doc assumes single root.
- **Sidecar GC** (sidecars for torrents removed from qBittorrent). Doc
  recommends a separate `tql sidecar gc` command, not automatic.
- **Cross-seed integration.** `cross_seed` origin reserved.
- **Scripting language alternatives.** Rhai is chosen for Rust-native
  embedding. Lua (`mlua`) and Starlark (`starlark-rust`) are reasonable
  alternates if the user base shifts. The sandbox model and the
  manifest-derived schema work the same for any of them.

---

## Glossary

- **Canonical tracker name**: the tracker's web-facing domain, lowercased,
  no scheme, no path. Hard-coded in the manifest's `canonical_category`.
- **Classify function**: the pure Rhai function that maps validated input
  to `{link_tags, info_tags, warnings}`. Cannot do I/O. Cannot set the
  category.
- **Manifest**: `manifest.toml` declaring a tracker's metadata and input
  schema. Drives MCP, REST, and CLI surfaces.
- **Link site / link tag**: a `link:` tag in qBittorrent and the
  corresponding hardlinked location in the library tree.
- **Multi-homing**: the same physical file (inode) at multiple paths via
  hardlinks.
- **Sidecar**: per-torrent JSON file recording the link sites the
  post-processor has created.
- **ATM**: qBittorrent's Automatic Torrent Management.
- **Reconciliation**: bringing the filesystem into agreement with current
  qBittorrent tag state.
