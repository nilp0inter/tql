//! Configuration loading per DESIGN.md §11.
//!
//! TOML on disk, env overrides via `TQL_` prefix using double-underscore as the
//! section separator (`TQL_PATHS__SEED_ROOT` → `paths.seed_root`). Secrets are
//! never inlined: only `*_env` names are stored, and the consumer is expected
//! to look up the actual value at use time.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use figment::providers::{Env, Format, Toml};
use figment::Figment;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    pub paths: Paths,
    #[serde(default)]
    pub linking: Linking,
    #[serde(default)]
    pub qbittorrent: Option<QBittorrent>,
    #[serde(default)]
    pub notify: Notify,
    #[serde(default)]
    pub media: Media,
    #[serde(default)]
    pub reconcile: Reconcile,
    #[serde(default)]
    pub mcp: Mcp,
    #[serde(default)]
    pub api: Api,
    #[serde(default)]
    pub scripting: Scripting,
    #[serde(default)]
    pub trackers: BTreeMap<String, TrackerCreds>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Paths {
    pub seed_root: PathBuf,
    pub library_root: PathBuf,
    pub trackers_root: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Linking {
    #[serde(default = "default_link_prefer")]
    pub prefer: LinkPrefer,
    #[serde(default)]
    pub windows_compat: bool,
}

impl Default for Linking {
    fn default() -> Self {
        Self {
            prefer: default_link_prefer(),
            windows_compat: false,
        }
    }
}

fn default_link_prefer() -> LinkPrefer {
    LinkPrefer::Hardlink
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkPrefer {
    Hardlink,
    Reflink,
    ReflinkOrHardlink,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct QBittorrent {
    pub url: String,
    pub username: String,
    pub password_env: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Notify {
    #[serde(default)]
    pub default: Vec<String>,
    /// Override the JSONL spool path. Default: `<library_root>/.metadata/notify.spool`.
    #[serde(default)]
    pub spool_path: Option<PathBuf>,
    /// Override the embedded `notify.rhai` field-manipulation script.
    /// Absolute path; the file must exist and compile under the sandbox.
    /// On failure the drainer logs WARN and falls back to the embedded
    /// default (DESIGN §15.2).
    #[serde(default)]
    pub script_path: Option<PathBuf>,
    /// Override the embedded `notify.hbs` Handlebars template. Same
    /// resolution & fallback semantics as `script_path`.
    #[serde(default)]
    pub template_path: Option<PathBuf>,
    #[serde(default)]
    pub telegram: Option<Telegram>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Telegram {
    pub bot_token_env: String,
    pub chat_id: String,
    #[serde(default = "default_parse_mode")]
    pub parse_mode: String,
    /// Override the Bot API base URL. Defaults to `https://api.telegram.org`.
    /// Primarily a test seam (point at a local HTTP sink in NixOS checks);
    /// production deployments should leave it unset.
    #[serde(default)]
    pub base_url: Option<String>,
}

fn default_parse_mode() -> String {
    "HTML".into()
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Media {
    #[serde(default)]
    pub plex: Option<Plex>,
    #[serde(default)]
    pub jellyfin: Option<Jellyfin>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Plex {
    pub url: String,
    pub token_env: String,
    #[serde(default)]
    pub section_ids: Vec<u32>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Jellyfin {
    pub url: String,
    pub api_key_env: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Reconcile {
    #[serde(default = "default_parallelism")]
    pub parallelism: u32,
    #[serde(default = "yes")]
    pub adopt_orphans: bool,
    #[serde(default = "yes")]
    pub remove_stale: bool,
}

impl Default for Reconcile {
    fn default() -> Self {
        Self {
            parallelism: default_parallelism(),
            adopt_orphans: true,
            remove_stale: true,
        }
    }
}

fn default_parallelism() -> u32 {
    4
}
fn yes() -> bool {
    true
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Mcp {
    #[serde(default = "default_mcp_transport")]
    pub transport: McpTransport,
    #[serde(default = "default_mcp_addr")]
    pub http_addr: String,
    #[serde(default)]
    pub api_key_env: Option<String>,
}

impl Default for Mcp {
    fn default() -> Self {
        Self {
            transport: default_mcp_transport(),
            http_addr: default_mcp_addr(),
            api_key_env: None,
        }
    }
}

fn default_mcp_transport() -> McpTransport {
    McpTransport::Stdio
}
fn default_mcp_addr() -> String {
    "127.0.0.1:7373".into()
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum McpTransport {
    Stdio,
    Http,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Api {
    #[serde(default = "default_api_addr")]
    pub addr: String,
    #[serde(default)]
    pub api_key_env: Option<String>,
}

impl Default for Api {
    fn default() -> Self {
        Self {
            addr: default_api_addr(),
            api_key_env: None,
        }
    }
}

fn default_api_addr() -> String {
    "127.0.0.1:7374".into()
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Scripting {
    #[serde(default)]
    pub reload_on_change: bool,
    #[serde(default = "default_runtime_ms")]
    pub max_script_runtime_ms: u64,
    #[serde(default = "default_memory_mb")]
    pub max_script_memory_mb: u64,
}

impl Default for Scripting {
    fn default() -> Self {
        Self {
            reload_on_change: false,
            max_script_runtime_ms: default_runtime_ms(),
            max_script_memory_mb: default_memory_mb(),
        }
    }
}

fn default_runtime_ms() -> u64 {
    200
}
fn default_memory_mb() -> u64 {
    16
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct TrackerCreds {
    #[serde(default)]
    pub cookie_env: Option<String>,
    #[serde(default)]
    pub auth_header_env: Option<String>,
}

/// Default config file locations, in priority order.
///
/// 1. `$TQL_CONFIG` (env var) if set.
/// 2. `$XDG_CONFIG_HOME/tql/config.toml` (or `~/.config/tql/config.toml`).
/// 3. `/etc/tql/config.toml`.
pub fn default_search_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(p) = std::env::var("TQL_CONFIG") {
        paths.push(PathBuf::from(p));
    }
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
    if let Some(base) = xdg {
        paths.push(base.join("tql").join("config.toml"));
    }
    paths.push(PathBuf::from("/etc/tql/config.toml"));
    paths
}

/// Resolve the config file path: explicit override first, then search paths.
pub fn resolve_path(explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = explicit {
        return Some(p.to_path_buf());
    }
    default_search_paths().into_iter().find(|p| p.exists())
}

/// Load and parse the config. Returns an error string suitable for printing.
pub fn load(explicit: Option<&Path>) -> Result<(PathBuf, Config), String> {
    let path = resolve_path(explicit).ok_or_else(|| {
        "no config file found (set $TQL_CONFIG or create ~/.config/tql/config.toml)".to_string()
    })?;

    let fig = Figment::new()
        .merge(Toml::file(&path))
        .merge(Env::prefixed("TQL_").split("__"));

    let cfg: Config = fig.extract().map_err(|e| format!("config error: {e}"))?;
    Ok((path, cfg))
}
