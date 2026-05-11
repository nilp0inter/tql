use std::path::PathBuf;

use clap::Parser;

use crate::config::{self, Config};
use crate::sidecar;

#[derive(Parser, Debug)]
pub struct Args {
    /// Info hash (v1, lowercase hex) of the torrent.
    pub hash: String,

    /// Explicit config file path; overrides the default search.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
}

pub fn run(args: Args) -> Result<(), u8> {
    let (_path, cfg) = match config::load(args.config.as_deref()) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("sidecar show: {e}");
            return Err(1);
        }
    };
    show(&cfg, &args.hash, &mut std::io::stdout())
}

pub(crate) fn show<W: std::io::Write>(cfg: &Config, hash: &str, out: &mut W) -> Result<(), u8> {
    let path = sidecar::sidecar_path(&cfg.paths.library_root, hash);
    match sidecar::read(&path) {
        Ok(Some(sc)) => match serde_json::to_string_pretty(&sc) {
            Ok(s) => {
                let _ = writeln!(out, "{s}");
                Ok(())
            }
            Err(e) => {
                eprintln!("sidecar show: serialize failed: {e}");
                Err(1)
            }
        },
        Ok(None) => {
            eprintln!("sidecar show: no sidecar at {}", path.display());
            Err(1)
        }
        Err(e) => {
            eprintln!("sidecar show: {e}");
            Err(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::config::{
        Api, Linking, Mcp, Media, Notify, Paths, QBittorrent, Reconcile, Scripting,
    };
    use crate::sidecar::{Origin, Sidecar, SCHEMA_VERSION};

    fn tmp_dir(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        let pid = std::process::id();
        let nano = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        p.push(format!("tql-show-{tag}-{pid}-{nano}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn cfg_with_lib(library_root: PathBuf) -> Config {
        Config {
            paths: Paths {
                seed_root: library_root.clone(),
                library_root,
                trackers_root: PathBuf::from("/nonexistent"),
            },
            linking: Linking::default(),
            qbittorrent: None::<QBittorrent>,
            notify: Notify::default(),
            media: Media::default(),
            reconcile: Reconcile::default(),
            mcp: Mcp::default(),
            api: Api::default(),
            scripting: Scripting::default(),
            trackers: Default::default(),
        }
    }

    fn sample(hash: &str) -> Sidecar {
        Sidecar {
            schema_version: SCHEMA_VERSION,
            info_hash_v1: hash.to_string(),
            info_hash_v2: None,
            name: "Build".into(),
            category: "myanonamouse.net".into(),
            content_path: "/seed/myanonamouse.net/Build".into(),
            is_directory: true,
            size_bytes: 4_760_000,
            link_sites: vec![],
            last_applied_tags: vec![],
            last_applied_at: None,
            warnings: vec![],
        }
    }

    #[test]
    fn shows_existing_sidecar_as_json() {
        let lib = tmp_dir("ok");
        let hash = "abc123";
        let p = sidecar::sidecar_path(&lib, hash);
        sidecar::write(&p, &sample(hash)).unwrap();

        let cfg = cfg_with_lib(lib);
        let mut buf: Vec<u8> = Vec::new();
        let r = show(&cfg, hash, &mut buf);
        assert!(r.is_ok());

        let printed = String::from_utf8(buf).unwrap();
        assert!(printed.contains("\"info_hash_v1\": \"abc123\""));
        assert!(printed.contains("\"category\": \"myanonamouse.net\""));
        // Round-trip: the printed JSON must parse back into a Sidecar.
        let parsed: Sidecar = serde_json::from_str(&printed).unwrap();
        assert_eq!(parsed.info_hash_v1, "abc123");
    }

    #[test]
    fn missing_sidecar_is_error() {
        let lib = tmp_dir("missing");
        let cfg = cfg_with_lib(lib);
        let mut buf: Vec<u8> = Vec::new();
        let r = show(&cfg, "deadbeef", &mut buf);
        assert_eq!(r, Err(1));
        assert!(buf.is_empty());
    }

    #[test]
    fn malformed_sidecar_is_error() {
        let lib = tmp_dir("bad");
        let hash = "deadbeef";
        let p = sidecar::sidecar_path(&lib, hash);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, b"not json").unwrap();

        let cfg = cfg_with_lib(lib);
        let mut buf: Vec<u8> = Vec::new();
        let r = show(&cfg, hash, &mut buf);
        assert_eq!(r, Err(1));
    }

    #[test]
    fn origin_round_trips_in_output() {
        let lib = tmp_dir("origin");
        let hash = "feed";
        let p = sidecar::sidecar_path(&lib, hash);
        let mut sc = sample(hash);
        sc.link_sites.push(crate::sidecar::LinkSite {
            relative_path: "Cat/Sub".into(),
            resolved_path: "/lib/myanonamouse.net/Cat/Sub/Build".into(),
            created_at: "2026-05-11T00:00:00Z".into(),
            origin: Origin::PostProcess,
        });
        sidecar::write(&p, &sc).unwrap();

        let cfg = cfg_with_lib(lib);
        let mut buf: Vec<u8> = Vec::new();
        show(&cfg, hash, &mut buf).unwrap();
        let printed = String::from_utf8(buf).unwrap();
        assert!(printed.contains("\"origin\": \"post_process\""));
    }
}
