use std::path::PathBuf;

use clap::Parser;
use serde_json::json;

use crate::config;
use crate::scripting::fixtures::{run_all, FixtureFailure, FixtureFailureKind};
use crate::scripting::registry::load_dir;
use crate::scripting::sandbox::{build_engine, SandboxLimits};

#[derive(Parser, Debug)]
pub struct Args {
    /// Limit to one tracker.
    pub tracker: Option<String>,

    /// Explicit config file path; overrides the default search.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Emit a single JSON document describing the run instead of human lines.
    #[arg(long)]
    pub json: bool,
}

pub fn run(args: Args) -> Result<(), u8> {
    let (_path, cfg) = match config::load(args.config.as_deref()) {
        Ok(v) => v,
        Err(e) => {
            if args.json {
                let v = json!({"error": format!("{e}")});
                println!("{}", serde_json::to_string_pretty(&v).unwrap());
            } else {
                eprintln!("test: {e}");
            }
            return Err(1);
        }
    };

    let limits = SandboxLimits::default();
    let engine = build_engine(&limits);

    let report = match load_dir(&cfg.paths.trackers_root, &engine) {
        Ok(r) => r,
        Err(e) => {
            if args.json {
                let v = json!({"error": format!("{e}")});
                println!("{}", serde_json::to_string_pretty(&v).unwrap());
            } else {
                eprintln!("test: {e}");
            }
            return Err(1);
        }
    };

    if let Some(name) = &args.tracker {
        if report.registry.get(name).is_none() {
            if args.json {
                let v = json!({"error": format!("tracker {name:?} not found in registry")});
                println!("{}", serde_json::to_string_pretty(&v).unwrap());
            } else {
                eprintln!("test: tracker {name:?} not found in registry");
            }
            return Err(1);
        }
    }

    let load_failures: Vec<String> = report.failures.iter().map(|f| format!("{f}")).collect();
    let (total, failures) = run_all(&engine, &report.registry, args.tracker.as_deref());
    let passed = total - failures.len();
    let bad = !failures.is_empty() || !load_failures.is_empty();

    if args.json {
        println!(
            "{}",
            render_json(
                &report.registry.len(),
                &load_failures,
                total,
                passed,
                &failures,
                bad
            )
        );
    } else {
        for f in &report.failures {
            eprintln!("test: load failure: {f}");
        }
        for fail in &failures {
            eprintln!("FAIL {fail}");
        }
        eprintln!(
            "test: {} tracker(s) loaded, {} fixture(s) total, {} passed, {} failed",
            report.registry.len(),
            total,
            passed,
            failures.len(),
        );
    }

    if bad {
        return Err(1);
    }
    Ok(())
}

fn failure_kind_str(k: &FixtureFailureKind) -> &'static str {
    match k {
        FixtureFailureKind::Io(_) => "io",
        FixtureFailureKind::Parse(_) => "parse",
        FixtureFailureKind::Input(_) => "input",
        FixtureFailureKind::Classify(_) => "classify",
        FixtureFailureKind::Mismatch { .. } => "mismatch",
    }
}

pub(crate) fn render_json(
    trackers_loaded: &usize,
    load_failures: &[String],
    total: usize,
    passed: usize,
    failures: &[FixtureFailure],
    bad: bool,
) -> String {
    let failures_json: Vec<serde_json::Value> = failures
        .iter()
        .map(|f| {
            json!({
                "tracker": f.tracker,
                "fixture": f.fixture.display().to_string(),
                "kind": failure_kind_str(&f.kind),
                "message": format!("{f}"),
            })
        })
        .collect();
    let v = json!({
        "trackers_loaded": trackers_loaded,
        "load_failures": load_failures,
        "summary": {
            "total": total,
            "passed": passed,
            "failed": failures.len(),
        },
        "failures": failures_json,
        "exit_code": if bad { 1 } else { 0 },
    });
    serde_json::to_string_pretty(&v).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    fn write(path: &Path, contents: &str) {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    fn tmp_dir(prefix: &str) -> PathBuf {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!("{prefix}-{pid}-{nanos}"));
        fs::create_dir_all(&p).unwrap();
        p
    }

    const MANIFEST: &str = r#"
name = "demo"
canonical_category = "demo.example"
description = "demo"

[[input]]
name = "cat"
type = "string"
required = true
"#;

    const SCRIPT: &str = r#"
fn classify(input) {
    #{ link_tags: ["link:" + input.cat], info_tags: [], warnings: [] }
}
"#;

    fn write_world(root: &Path, fixture: &str) -> PathBuf {
        let trackers = root.join("trackers");
        write(&trackers.join("demo/manifest.toml"), MANIFEST);
        write(&trackers.join("demo/classify.rhai"), SCRIPT);
        write(&trackers.join("demo/fixtures/case.toml"), fixture);
        let cfg_path = root.join("config.toml");
        let lib = root.join("lib");
        let seed = root.join("seed");
        fs::create_dir_all(&lib).unwrap();
        fs::create_dir_all(&seed).unwrap();
        let cfg_body = format!(
            r#"
[paths]
seed_root = "{}"
library_root = "{}"
trackers_root = "{}"
"#,
            seed.display(),
            lib.display(),
            trackers.display(),
        );
        write(&cfg_path, &cfg_body);
        cfg_path
    }

    #[test]
    fn json_mode_passes() {
        let root = tmp_dir("tql-test-json-pass");
        let cfg = write_world(
            &root,
            r#"
[input]
cat = "Foo"

[expected_output]
link_tags = ["link:Foo"]
info_tags = []
warnings = []
"#,
        );
        let r = run(Args {
            tracker: None,
            config: Some(cfg),
            json: true,
        });
        assert!(r.is_ok());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn json_mode_fails_on_mismatch() {
        let root = tmp_dir("tql-test-json-fail");
        let cfg = write_world(
            &root,
            r#"
[input]
cat = "Foo"

[expected_output]
link_tags = ["link:Bar"]
"#,
        );
        let r = run(Args {
            tracker: None,
            config: Some(cfg),
            json: true,
        });
        assert_eq!(r, Err(1));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn render_json_shape() {
        let s = render_json(&3, &[], 5, 5, &[], false);
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["trackers_loaded"], 3);
        assert_eq!(v["summary"]["total"], 5);
        assert_eq!(v["summary"]["passed"], 5);
        assert_eq!(v["summary"]["failed"], 0);
        assert_eq!(v["exit_code"], 0);
        assert!(v["failures"].as_array().unwrap().is_empty());
        assert!(v["load_failures"].as_array().unwrap().is_empty());
    }

    #[test]
    fn render_json_with_load_failures_marks_bad() {
        let s = render_json(
            &0,
            &["bad-tracker: missing manifest".to_string()],
            0,
            0,
            &[],
            true,
        );
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["exit_code"], 1);
        assert_eq!(
            v["load_failures"][0].as_str().unwrap(),
            "bad-tracker: missing manifest"
        );
    }
}
