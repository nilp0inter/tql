//! `tql doctor` — installation health check (Leg 16a).
//!
//! Walks a checklist of static + runtime invariants per DESIGN.md §7. Each
//! check resolves to OK / WARN / FAIL; FAILs make the command exit non-zero.
//! WARNs are advisory (config gaps that don't block operation).

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use clap::Parser;

use crate::config::{self, Config};
use crate::scripting::fixtures::run_all;
use crate::scripting::registry::load_dir;
use crate::scripting::sandbox::{build_engine, SandboxLimits};

#[derive(Parser, Debug)]
pub struct Args {
    /// Send a test notification and probe each media server.
    #[arg(long)]
    pub probe: bool,

    /// Explicit config file path; overrides the default search.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Status {
    Ok(String),
    Warn(String),
    Fail(String),
}

#[derive(Debug)]
pub struct Check {
    pub name: String,
    pub status: Status,
}

pub fn run(args: Args) -> Result<(), u8> {
    let mut checks: Vec<Check> = Vec::new();

    let (path, cfg) = match config::load(args.config.as_deref()) {
        Ok(v) => {
            checks.push(Check {
                name: "config".into(),
                status: Status::Ok(format!("parsed {}", v.0.display())),
            });
            v
        }
        Err(e) => {
            checks.push(Check {
                name: "config".into(),
                status: Status::Fail(e),
            });
            return finish(checks);
        }
    };
    let _ = path;

    checks.extend(check_paths(&cfg));
    checks.extend(check_trackers(&cfg));
    checks.extend(check_qbittorrent(&cfg));
    if args.probe {
        checks.extend(probe_notify(&cfg));
        checks.extend(probe_media(&cfg));
    } else {
        checks.push(Check {
            name: "probe".into(),
            status: Status::Warn("skipped (pass --probe to test notify + media servers)".into()),
        });
    }

    finish(checks)
}

fn finish(checks: Vec<Check>) -> Result<(), u8> {
    let mut fails = 0usize;
    let mut warns = 0usize;
    for c in &checks {
        let (tag, body) = match &c.status {
            Status::Ok(b) => ("OK  ", b.as_str()),
            Status::Warn(b) => {
                warns += 1;
                ("WARN", b.as_str())
            }
            Status::Fail(b) => {
                fails += 1;
                ("FAIL", b.as_str())
            }
        };
        println!("[{tag}] {:24} {body}", c.name);
    }
    println!();
    println!(
        "doctor: {} check(s) — {} ok, {} warn, {} fail",
        checks.len(),
        checks.len() - warns - fails,
        warns,
        fails
    );
    if fails > 0 {
        return Err(1);
    }
    Ok(())
}

fn check_paths(cfg: &Config) -> Vec<Check> {
    let mut out = Vec::new();
    let seed = &cfg.paths.seed_root;
    let lib = &cfg.paths.library_root;

    out.push(check_dir("paths.seed_root", seed));
    out.push(check_dir("paths.library_root", lib));

    out.push(match (fs::metadata(seed), fs::metadata(lib)) {
        (Ok(s), Ok(l)) if s.dev() == l.dev() => Check {
            name: "paths.same_fs".into(),
            status: Status::Ok(format!("st_dev = {}", s.dev())),
        },
        (Ok(s), Ok(l)) => Check {
            name: "paths.same_fs".into(),
            status: Status::Fail(format!(
                "seed_root st_dev={} != library_root st_dev={} (link(2)/reflink will EXDEV)",
                s.dev(),
                l.dev()
            )),
        },
        _ => Check {
            name: "paths.same_fs".into(),
            status: Status::Warn("could not stat one of the roots; see prior failures".into()),
        },
    });

    let metadata_dir = lib.join(".metadata");
    out.push(match fs::create_dir_all(&metadata_dir) {
        Ok(()) => match writable(&metadata_dir) {
            Ok(()) => Check {
                name: "paths.metadata_dir".into(),
                status: Status::Ok(format!("{} writable", metadata_dir.display())),
            },
            Err(e) => Check {
                name: "paths.metadata_dir".into(),
                status: Status::Fail(format!("{} not writable: {e}", metadata_dir.display())),
            },
        },
        Err(e) => Check {
            name: "paths.metadata_dir".into(),
            status: Status::Fail(format!("could not create {}: {e}", metadata_dir.display())),
        },
    });

    out
}

fn check_dir(label: &str, p: &Path) -> Check {
    match fs::metadata(p) {
        Ok(m) if m.is_dir() => Check {
            name: label.into(),
            status: Status::Ok(format!("{} (dir)", p.display())),
        },
        Ok(_) => Check {
            name: label.into(),
            status: Status::Fail(format!("{} exists but is not a directory", p.display())),
        },
        Err(e) => Check {
            name: label.into(),
            status: Status::Fail(format!("{}: {e}", p.display())),
        },
    }
}

fn writable(dir: &Path) -> std::io::Result<()> {
    let probe = dir.join(".tql-doctor-write-probe");
    fs::write(&probe, b"")?;
    fs::remove_file(&probe)?;
    Ok(())
}

fn check_trackers(cfg: &Config) -> Vec<Check> {
    let mut out = Vec::new();
    let limits = SandboxLimits::default();
    let engine = build_engine(&limits);

    let report = match load_dir(&cfg.paths.trackers_root, &engine) {
        Ok(r) => r,
        Err(e) => {
            out.push(Check {
                name: "trackers.root".into(),
                status: Status::Fail(format!("{}: {e}", cfg.paths.trackers_root.display())),
            });
            return out;
        }
    };
    out.push(Check {
        name: "trackers.root".into(),
        status: Status::Ok(format!(
            "{} ({} loaded, {} failed)",
            cfg.paths.trackers_root.display(),
            report.registry.len(),
            report.failures.len()
        )),
    });
    for f in &report.failures {
        out.push(Check {
            name: format!("trackers.load[{}]", short(&f.to_string())),
            status: Status::Fail(f.to_string()),
        });
    }

    let (total, failures) = run_all(&engine, &report.registry, None);
    let passed = total - failures.len();
    out.push(Check {
        name: "trackers.fixtures".into(),
        status: if failures.is_empty() {
            Status::Ok(format!("{passed}/{total} fixture(s) passed"))
        } else {
            Status::Fail(format!("{passed}/{total} fixture(s) passed"))
        },
    });
    for f in &failures {
        out.push(Check {
            name: format!("trackers.fixture[{}]", f.tracker),
            status: Status::Fail(f.to_string()),
        });
    }
    out
}

fn short(s: &str) -> String {
    s.chars().take(40).collect()
}

fn check_qbittorrent(cfg: &Config) -> Vec<Check> {
    let q = match &cfg.qbittorrent {
        Some(q) => q,
        None => {
            return vec![Check {
                name: "qbittorrent".into(),
                status: Status::Warn("not configured ([qbittorrent] missing)".into()),
            }];
        }
    };

    let password = match std::env::var(&q.password_env) {
        Ok(p) => p,
        Err(_) => {
            return vec![Check {
                name: "qbittorrent.auth".into(),
                status: Status::Fail(format!("env {} not set", q.password_env)),
            }];
        }
    };

    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            return vec![Check {
                name: "qbittorrent".into(),
                status: Status::Fail(format!("tokio init: {e}")),
            }];
        }
    };

    rt.block_on(async {
        let client = match crate::qbit::Client::new(&q.url) {
            Ok(c) => c,
            Err(e) => {
                return vec![Check {
                    name: "qbittorrent.client".into(),
                    status: Status::Fail(format!("{e}")),
                }];
            }
        };
        let mut out = Vec::new();
        match client.login(&q.username, &password).await {
            Ok(()) => out.push(Check {
                name: "qbittorrent.login".into(),
                status: Status::Ok(format!("{} as {}", q.url, q.username)),
            }),
            Err(e) => {
                out.push(Check {
                    name: "qbittorrent.login".into(),
                    status: Status::Fail(format!("{e}")),
                });
                return out;
            }
        }
        match client.app_version().await {
            Ok(v) => out.push(Check {
                name: "qbittorrent.version".into(),
                status: Status::Ok(v),
            }),
            Err(e) => out.push(Check {
                name: "qbittorrent.version".into(),
                status: Status::Fail(format!("{e}")),
            }),
        }
        out
    })
}

fn probe_notify(cfg: &Config) -> Vec<Check> {
    let tg = match &cfg.notify.telegram {
        Some(t) => t,
        None => {
            return vec![Check {
                name: "notify.telegram".into(),
                status: Status::Warn("not configured".into()),
            }];
        }
    };
    let token = match std::env::var(&tg.bot_token_env) {
        Ok(v) => v,
        Err(_) => {
            return vec![Check {
                name: "notify.telegram".into(),
                status: Status::Fail(format!("env {} not set", tg.bot_token_env)),
            }];
        }
    };
    let url = format!("https://api.telegram.org/bot{token}/getMe");
    vec![http_probe("notify.telegram", &url, None)]
}

fn probe_media(cfg: &Config) -> Vec<Check> {
    let mut out = Vec::new();
    match &cfg.media.plex {
        None => out.push(Check {
            name: "media.plex".into(),
            status: Status::Warn("not configured".into()),
        }),
        Some(p) => {
            let token = match std::env::var(&p.token_env) {
                Ok(v) => Some(v),
                Err(_) => {
                    out.push(Check {
                        name: "media.plex".into(),
                        status: Status::Fail(format!("env {} not set", p.token_env)),
                    });
                    return out;
                }
            };
            let url = format!("{}/identity", p.url.trim_end_matches('/'));
            out.push(http_probe(
                "media.plex",
                &url,
                token.as_deref().map(|t| ("X-Plex-Token", t.to_string())),
            ));
        }
    }
    match &cfg.media.jellyfin {
        None => out.push(Check {
            name: "media.jellyfin".into(),
            status: Status::Warn("not configured".into()),
        }),
        Some(j) => {
            // /System/Info/Public is unauthenticated by design; just verify
            // the env var exists and the host responds.
            if std::env::var(&j.api_key_env).is_err() {
                out.push(Check {
                    name: "media.jellyfin".into(),
                    status: Status::Fail(format!("env {} not set", j.api_key_env)),
                });
            } else {
                let url = format!("{}/System/Info/Public", j.url.trim_end_matches('/'));
                out.push(http_probe("media.jellyfin", &url, None));
            }
        }
    }
    out
}

fn http_probe(name: &str, url: &str, header: Option<(&str, String)>) -> Check {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            return Check {
                name: name.into(),
                status: Status::Fail(format!("tokio init: {e}")),
            };
        }
    };
    let url_owned = url.to_string();
    let header_owned = header.map(|(k, v)| (k.to_string(), v));
    rt.block_on(async move {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build();
        let client = match client {
            Ok(c) => c,
            Err(e) => {
                return Check {
                    name: name.into(),
                    status: Status::Fail(format!("client init: {e}")),
                };
            }
        };
        let mut req = client.get(&url_owned);
        if let Some((k, v)) = &header_owned {
            req = req.header(k.as_str(), v.as_str());
        }
        match req.send().await {
            Ok(r) if r.status().is_success() => Check {
                name: name.into(),
                status: Status::Ok(format!("{} → HTTP {}", url_owned, r.status())),
            },
            Ok(r) => Check {
                name: name.into(),
                status: Status::Fail(format!("{} → HTTP {}", url_owned, r.status())),
            },
            Err(e) => Check {
                name: name.into(),
                status: Status::Fail(format!("{url_owned}: {e}")),
            },
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let mut d = std::env::temp_dir();
        let unique = format!(
            "tql-doctor-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        d.push(unique);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn make_cfg(seed: &Path, lib: &Path, trackers: &Path) -> Config {
        let toml_text = format!(
            r#"
[paths]
seed_root = "{}"
library_root = "{}"
trackers_root = "{}"
"#,
            seed.display(),
            lib.display(),
            trackers.display()
        );
        toml::from_str(&toml_text).unwrap()
    }

    #[test]
    fn paths_ok_when_all_present_same_fs() {
        let root = tmp();
        let seed = root.join("seed");
        let lib = root.join("lib");
        let trackers = root.join("trackers");
        for p in [&seed, &lib, &trackers] {
            std::fs::create_dir_all(p).unwrap();
        }
        let cfg = make_cfg(&seed, &lib, &trackers);
        let checks = check_paths(&cfg);
        for c in &checks {
            assert!(matches!(c.status, Status::Ok(_)), "{:?}", c);
        }
    }

    #[test]
    fn paths_fail_when_seed_missing() {
        let root = tmp();
        let lib = root.join("lib");
        let trackers = root.join("trackers");
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::create_dir_all(&trackers).unwrap();
        let cfg = make_cfg(&root.join("nope"), &lib, &trackers);
        let checks = check_paths(&cfg);
        assert!(checks.iter().any(|c| matches!(c.status, Status::Fail(_))));
    }

    #[test]
    fn trackers_check_runs_fixtures() {
        let root = tmp();
        let seed = root.join("seed");
        let lib = root.join("lib");
        for p in [&seed, &lib] {
            std::fs::create_dir_all(p).unwrap();
        }
        // Use the in-tree example tracker.
        let trackers = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("trackers");
        let cfg = make_cfg(&seed, &lib, &trackers);
        let checks = check_trackers(&cfg);
        assert!(checks.iter().any(|c| c.name == "trackers.root"));
        assert!(checks.iter().any(|c| c.name == "trackers.fixtures"
            && matches!(c.status, Status::Ok(_))));
    }

    #[test]
    fn qbittorrent_warn_when_unconfigured() {
        let root = tmp();
        let seed = root.join("seed");
        let lib = root.join("lib");
        let trackers = root.join("trackers");
        for p in [&seed, &lib, &trackers] {
            std::fs::create_dir_all(p).unwrap();
        }
        let cfg = make_cfg(&seed, &lib, &trackers);
        let checks = check_qbittorrent(&cfg);
        assert!(matches!(checks[0].status, Status::Warn(_)));
    }
}
