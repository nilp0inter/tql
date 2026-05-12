//! `tql config show` — print the effective configuration and the file it was
//! loaded from.
//!
//! Operators driving `tql` from automation regularly need to know *which*
//! config file actually got picked up (the search order is documented but
//! environment-dependent) and *what* the parsed values look like after the
//! `$TQL_*` env-overlay pass. Until now the only way to find out was to read
//! `src/config.rs` or run `doctor`, neither of which surfaces the full
//! effective tree. This subcommand fills that gap.
//!
//! No secret values are stored in `Config` — only env-var *names* (e.g.
//! `api_key_env = "TQL_API_KEY"`), so printing is always safe.

use std::path::PathBuf;

use clap::Parser;

use crate::config::{self, Config};

#[derive(Parser, Debug)]
pub struct Args {
    /// Explicit config file path; overrides the default search.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Print only the resolved config file path, then exit.
    #[arg(long)]
    pub path_only: bool,
}

pub fn run(args: Args) -> Result<(), u8> {
    let (path, cfg) = match config::load(args.config.as_deref()) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("config show: {e}");
            return Err(1);
        }
    };
    show(&path, &cfg, args.path_only, &mut std::io::stdout())
}

pub(crate) fn show<W: std::io::Write>(
    path: &std::path::Path,
    cfg: &Config,
    path_only: bool,
    out: &mut W,
) -> Result<(), u8> {
    if path_only {
        let _ = writeln!(out, "{}", path.display());
        return Ok(());
    }
    let value = serde_json::json!({
        "path": path.display().to_string(),
        "config": cfg,
    });
    match serde_json::to_string_pretty(&value) {
        Ok(s) => {
            let _ = writeln!(out, "{s}");
            Ok(())
        }
        Err(e) => {
            eprintln!("config show: serialize failed: {e}");
            Err(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn sample_cfg() -> Config {
        use crate::config::{
            Api, Linking, Mcp, Media, Notify, Paths, QBittorrent, Reconcile, Scripting,
        };
        Config {
            paths: Paths {
                seed_root: PathBuf::from("/seed"),
                library_root: PathBuf::from("/lib"),
                trackers_root: PathBuf::from("/trackers"),
            },
            linking: Linking::default(),
            qbittorrent: Some(QBittorrent {
                url: "http://127.0.0.1:8080".into(),
                username: "admin".into(),
                password_env: "QBIT_PASS".into(),
            }),
            notify: Notify::default(),
            media: Media::default(),
            reconcile: Reconcile::default(),
            mcp: Mcp::default(),
            api: Api::default(),
            scripting: Scripting::default(),
            trackers: Default::default(),
        }
    }

    #[test]
    fn show_path_only_prints_just_the_path() {
        let cfg = sample_cfg();
        let mut buf: Vec<u8> = Vec::new();
        show(Path::new("/etc/tql/config.toml"), &cfg, true, &mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert_eq!(out.trim(), "/etc/tql/config.toml");
    }

    #[test]
    fn show_default_emits_pretty_json_with_path_and_config() {
        let cfg = sample_cfg();
        let mut buf: Vec<u8> = Vec::new();
        show(Path::new("/etc/tql/config.toml"), &cfg, false, &mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["path"], "/etc/tql/config.toml");
        assert_eq!(v["config"]["paths"]["seed_root"], "/seed");
        assert_eq!(v["config"]["paths"]["library_root"], "/lib");
        assert_eq!(v["config"]["qbittorrent"]["url"], "http://127.0.0.1:8080");
        assert!(
            out.contains('\n'),
            "default output should be pretty-printed"
        );
    }

    #[test]
    fn show_does_not_leak_secret_values_only_env_names() {
        // Belt-and-braces guard: Config stores `*_env` names, never the values.
        let cfg = sample_cfg();
        let mut buf: Vec<u8> = Vec::new();
        show(Path::new("/etc/tql/config.toml"), &cfg, false, &mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("password_env"));
        assert!(out.contains("QBIT_PASS"));
        // The struct intentionally has no `password` field — only the env name.
        assert!(!out.contains("\"password\":"));
    }
}
