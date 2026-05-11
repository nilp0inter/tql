//! `tql reload` — signal a running `tql api` / `tql mcp` server to re-read
//! `<trackers_root>` (DESIGN.md §7).
//!
//! Workflow:
//!   1. Load config (`--config` honored).
//!   2. Validate the trackers tree by building an ephemeral registry; any
//!      top-level error is fatal here so we don't ask a live server to swap
//!      to a broken state. Per-tracker failures are reported but non-fatal —
//!      the server itself logs them too and will refuse the broken trackers.
//!   3. Look up `api.pid` and `mcp.pid` in the run directory. For each live
//!      PID, deliver SIGHUP.
//!   4. If no live server is found, print a warning and exit 0 (the design
//!      explicitly calls this a no-op + warning, not a failure).

use std::path::PathBuf;

use clap::Parser;

use crate::config;
use crate::pidfile;
use crate::scripting::registry::load_dir;
use crate::scripting::sandbox::{build_engine, SandboxLimits};

#[derive(Parser, Debug)]
pub struct Args {
    /// Explicit config file path; overrides the default search.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
    /// Skip the pre-flight registry validation.
    #[arg(long)]
    pub skip_validate: bool,
    /// Emit a single JSON document describing the outcome instead of human lines.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signal {
    pub role: String,
    pub pid: u32,
}

#[derive(Debug)]
pub enum Outcome {
    Ok {
        validated: Option<usize>,
        load_failures: Vec<String>,
        signaled: Vec<Signal>,
        errors: Vec<String>,
    },
    Error(String),
}

pub(crate) fn render_json(outcome: &Outcome) -> String {
    use serde_json::json;
    let v = match outcome {
        Outcome::Ok {
            validated,
            load_failures,
            signaled,
            errors,
        } => {
            let signaled_json: Vec<_> = signaled
                .iter()
                .map(|s| json!({"role": s.role, "pid": s.pid}))
                .collect();
            json!({
                "outcome": if errors.is_empty() { "ok" } else { "error" },
                "validated": validated,
                "load_failures": load_failures,
                "signaled": signaled_json,
                "errors": errors,
                "no_server": signaled.is_empty() && errors.is_empty(),
            })
        }
        Outcome::Error(msg) => json!({
            "outcome": "error",
            "message": msg,
        }),
    };
    serde_json::to_string_pretty(&v).unwrap()
}

fn do_run(args: &Args) -> Outcome {
    let (_path, cfg) = match config::load(args.config.as_deref()) {
        Ok(v) => v,
        Err(e) => return Outcome::Error(e.to_string()),
    };

    let mut validated: Option<usize> = None;
    let mut load_failures: Vec<String> = Vec::new();
    if !args.skip_validate {
        let engine = build_engine(&SandboxLimits::default());
        match load_dir(&cfg.paths.trackers_root, &engine) {
            Ok(report) => {
                for f in &report.failures {
                    load_failures.push(f.to_string());
                }
                validated = Some(report.registry.len());
            }
            Err(e) => {
                return Outcome::Error(format!("trackers_root unusable: {e}"));
            }
        }
    }

    let mut signaled = Vec::new();
    let mut errors = Vec::new();
    for role in ["api", "mcp"] {
        match pidfile::read(role) {
            Ok(Some(pid)) => match pidfile::send_sighup(pid) {
                Ok(()) => signaled.push(Signal {
                    role: role.into(),
                    pid,
                }),
                Err(e) => errors.push(format!("kill({pid}, SIGHUP) for {role}: {e}")),
            },
            Ok(None) => {}
            Err(e) => errors.push(format!("read {role}.pid: {e}")),
        }
    }

    Outcome::Ok {
        validated,
        load_failures,
        signaled,
        errors,
    }
}

pub fn run(args: Args) -> Result<(), u8> {
    let outcome = do_run(&args);
    let exit = match &outcome {
        Outcome::Error(_) => Err(1),
        Outcome::Ok { errors, .. } if !errors.is_empty() => Err(1),
        _ => Ok(()),
    };

    if args.json {
        println!("{}", render_json(&outcome));
    } else {
        match &outcome {
            Outcome::Error(msg) => eprintln!("reload: {msg}"),
            Outcome::Ok {
                validated,
                load_failures,
                signaled,
                errors,
            } => {
                for f in load_failures {
                    eprintln!("reload: load failure: {f}");
                }
                if let Some(n) = validated {
                    eprintln!("reload: validated {n} tracker(s)");
                }
                for s in signaled {
                    println!("reload: sent SIGHUP to {} (pid={})", s.role, s.pid);
                }
                for e in errors {
                    eprintln!("reload: {e}");
                }
                if signaled.is_empty() && errors.is_empty() {
                    eprintln!("reload: no running tql server found (no live PID files)");
                }
            }
        }
    }
    exit
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lock() -> std::sync::MutexGuard<'static, ()> {
        static M: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        M.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    fn isolate_run_dir() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "tql-reload-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        std::env::set_var("TQL_RUN_DIR", &d);
        d
    }

    fn write_min_config(dir: &std::path::Path) -> PathBuf {
        let trackers = dir.join("trackers");
        std::fs::create_dir_all(&trackers).unwrap();
        let cfg_path = dir.join("config.toml");
        let body = format!(
            r#"
[paths]
seed_root = "{seed}"
library_root = "{lib}"
trackers_root = "{trk}"
"#,
            seed = dir.display(),
            lib = dir.display(),
            trk = trackers.display()
        );
        std::fs::write(&cfg_path, body).unwrap();
        cfg_path
    }

    #[test]
    fn no_running_servers_yields_zero_exit_with_warning() {
        let _g = lock();
        let run_dir = isolate_run_dir();
        let cfg = write_min_config(&run_dir);
        let r = run(Args {
            config: Some(cfg),
            skip_validate: false,
            json: false,
        });
        assert_eq!(r, Ok(()));
        std::fs::remove_dir_all(&run_dir).ok();
    }

    #[test]
    fn stale_pid_file_is_treated_as_no_server() {
        let _g = lock();
        let run_dir = isolate_run_dir();
        let cfg = write_min_config(&run_dir);
        std::fs::write(run_dir.join("api.pid"), "2147483646").unwrap();
        let r = run(Args {
            config: Some(cfg),
            skip_validate: false,
            json: false,
        });
        assert_eq!(r, Ok(()));
        // Stale file should have been pruned by pidfile::read.
        assert!(!run_dir.join("api.pid").exists());
        std::fs::remove_dir_all(&run_dir).ok();
    }

    #[test]
    fn live_pid_receives_sighup_and_returns_ok() {
        let _g = lock();
        let run_dir = isolate_run_dir();
        let cfg = write_min_config(&run_dir);

        // Install a SIGHUP handler that flips a flag, then write our own PID
        // into api.pid; reload should deliver SIGHUP to us.
        static GOT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        extern "C" fn handler(_sig: libc::c_int) {
            GOT.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        let prev =
            unsafe { libc::signal(libc::SIGHUP, handler as *const () as libc::sighandler_t) };

        let pid = std::process::id();
        std::fs::write(run_dir.join("api.pid"), pid.to_string()).unwrap();
        let r = run(Args {
            config: Some(cfg),
            skip_validate: true,
            json: false,
        });
        assert_eq!(r, Ok(()));

        // Give the kernel a beat to deliver.
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(GOT.load(std::sync::atomic::Ordering::SeqCst));

        // Restore.
        unsafe { libc::signal(libc::SIGHUP, prev) };
        std::fs::remove_dir_all(&run_dir).ok();
    }

    #[test]
    fn render_json_no_server_shape() {
        let s = render_json(&Outcome::Ok {
            validated: Some(0),
            load_failures: vec![],
            signaled: vec![],
            errors: vec![],
        });
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["outcome"], "ok");
        assert_eq!(v["validated"], 0);
        assert_eq!(v["no_server"], true);
        assert!(v["signaled"].as_array().unwrap().is_empty());
        assert!(v["errors"].as_array().unwrap().is_empty());
        assert!(v["load_failures"].as_array().unwrap().is_empty());
    }

    #[test]
    fn render_json_signaled_shape() {
        let s = render_json(&Outcome::Ok {
            validated: None,
            load_failures: vec![],
            signaled: vec![Signal {
                role: "api".into(),
                pid: 42,
            }],
            errors: vec![],
        });
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["outcome"], "ok");
        assert!(v["validated"].is_null());
        assert_eq!(v["no_server"], false);
        let arr = v["signaled"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["role"], "api");
        assert_eq!(arr[0]["pid"], 42);
    }

    #[test]
    fn render_json_error_outcome_shape() {
        let v: serde_json::Value =
            serde_json::from_str(&render_json(&Outcome::Error("boom".into()))).unwrap();
        assert_eq!(v["outcome"], "error");
        assert_eq!(v["message"], "boom");
    }

    #[test]
    fn render_json_errors_field_flips_outcome_to_error() {
        let v: serde_json::Value = serde_json::from_str(&render_json(&Outcome::Ok {
            validated: Some(1),
            load_failures: vec![],
            signaled: vec![],
            errors: vec!["read api.pid: ENOENT".into()],
        }))
        .unwrap();
        assert_eq!(v["outcome"], "error");
        assert_eq!(v["errors"].as_array().unwrap().len(), 1);
    }
}
