use std::path::PathBuf;

use clap::Parser;

use crate::config;
use crate::scripting::fixtures::run_all;
use crate::scripting::registry::load_dir;
use crate::scripting::sandbox::{build_engine, SandboxLimits};

#[derive(Parser, Debug)]
pub struct Args {
    /// Limit to one tracker.
    pub tracker: Option<String>,

    /// Explicit config file path; overrides the default search.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
}

pub fn run(args: Args) -> Result<(), u8> {
    let (_path, cfg) = match config::load(args.config.as_deref()) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("test: {e}");
            return Err(1);
        }
    };

    let limits = SandboxLimits::default();
    let engine = build_engine(&limits);

    let report = match load_dir(&cfg.paths.trackers_root, &engine) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("test: {e}");
            return Err(1);
        }
    };

    let mut had_problem = false;
    for f in &report.failures {
        eprintln!("test: load failure: {f}");
        had_problem = true;
    }

    if let Some(name) = &args.tracker {
        if report.registry.get(name).is_none() {
            eprintln!("test: tracker {name:?} not found in registry");
            return Err(1);
        }
    }

    let (total, failures) = run_all(&engine, &report.registry, args.tracker.as_deref());

    for fail in &failures {
        eprintln!("FAIL {fail}");
    }

    let passed = total - failures.len();
    eprintln!(
        "test: {} tracker(s) loaded, {} fixture(s) total, {} passed, {} failed",
        report.registry.len(),
        total,
        passed,
        failures.len(),
    );

    if !failures.is_empty() || had_problem {
        return Err(1);
    }
    Ok(())
}
