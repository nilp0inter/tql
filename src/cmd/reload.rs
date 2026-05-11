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
}

pub fn run(args: Args) -> Result<(), u8> {
    let (_path, cfg) = match config::load(args.config.as_deref()) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("reload: {e}");
            return Err(1);
        }
    };

    if !args.skip_validate {
        let engine = build_engine(&SandboxLimits::default());
        match load_dir(&cfg.paths.trackers_root, &engine) {
            Ok(report) => {
                for f in &report.failures {
                    eprintln!("reload: load failure: {f}");
                }
                eprintln!(
                    "reload: validated {} tracker(s) under {}",
                    report.registry.len(),
                    cfg.paths.trackers_root.display(),
                );
            }
            Err(e) => {
                eprintln!("reload: trackers_root unusable: {e}");
                return Err(1);
            }
        }
    }

    let mut signaled = 0usize;
    let mut errors = 0usize;
    for role in ["api", "mcp"] {
        match pidfile::read(role) {
            Ok(Some(pid)) => match pidfile::send_sighup(pid) {
                Ok(()) => {
                    println!("reload: sent SIGHUP to {role} (pid={pid})");
                    signaled += 1;
                }
                Err(e) => {
                    eprintln!("reload: kill({pid}, SIGHUP) for {role}: {e}");
                    errors += 1;
                }
            },
            Ok(None) => {}
            Err(e) => {
                eprintln!("reload: read {role}.pid: {e}");
                errors += 1;
            }
        }
    }

    if signaled == 0 && errors == 0 {
        eprintln!("reload: no running tql server found (no live PID files)");
    }
    if errors > 0 {
        return Err(1);
    }
    Ok(())
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
        });
        assert_eq!(r, Ok(()));

        // Give the kernel a beat to deliver.
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(GOT.load(std::sync::atomic::Ordering::SeqCst));

        // Restore.
        unsafe { libc::signal(libc::SIGHUP, prev) };
        std::fs::remove_dir_all(&run_dir).ok();
    }
}
