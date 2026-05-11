//! Fixture runner for `tql test` (Leg 9b).
//!
//! Each tracker may carry a `fixtures/` subdirectory of TOML files; each
//! fixture has an `[input]` table (the same shape callers send to the
//! transport) and an `[expected_output]` table with `link_tags`,
//! `info_tags`, and `warnings`. The runner pipes `input` through
//! [`crate::scripting::input::marshal_input`] +
//! [`crate::scripting::host::run_classify`] and asserts equality against
//! `expected_output`.
//!
//! Discovery is non-recursive: only `<tracker>/fixtures/*.toml` files are
//! considered. Hidden files (leading `.`) are skipped. Missing
//! `fixtures/` is not an error — a tracker with no fixtures yields zero
//! results.

use std::fs;
use std::path::{Path, PathBuf};

use rhai::Engine;
use serde::Deserialize;
use serde_json::Value as Json;

use crate::scripting::host::run_classify;
use crate::scripting::input::{marshal_input, InputError};
use crate::scripting::registry::{Registry, Tracker};
use crate::scripting::types::{ClassifyError, ClassifyOutput};

/// `[expected_output]` portion of a fixture file.
#[derive(Debug, Deserialize, PartialEq, Eq)]
pub struct ExpectedOutput {
    #[serde(default)]
    pub link_tags: Vec<String>,
    #[serde(default)]
    pub info_tags: Vec<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct FixtureFile {
    input: toml::Value,
    expected_output: ExpectedOutput,
}

/// A loaded, parsed fixture awaiting execution.
#[derive(Debug)]
pub struct Fixture {
    pub path: PathBuf,
    pub input: Json,
    pub expected: ExpectedOutput,
}

/// One fixture-execution failure.
#[derive(Debug)]
pub struct FixtureFailure {
    pub tracker: String,
    pub fixture: PathBuf,
    pub kind: FixtureFailureKind,
}

#[derive(Debug)]
pub enum FixtureFailureKind {
    /// Couldn't read the file.
    Io(std::io::Error),
    /// Couldn't parse the TOML.
    Parse(toml::de::Error),
    /// Input didn't validate against the manifest.
    Input(InputError),
    /// The script errored.
    Classify(ClassifyError),
    /// The script ran but its output did not match `expected_output`.
    Mismatch {
        expected: ExpectedOutput,
        actual: ClassifyOutput,
    },
}

impl std::fmt::Display for FixtureFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} [{}]: ", self.tracker, self.fixture.display())?;
        match &self.kind {
            FixtureFailureKind::Io(e) => write!(f, "io error: {e}"),
            FixtureFailureKind::Parse(e) => write!(f, "parse error: {e}"),
            FixtureFailureKind::Input(e) => write!(f, "input error: {e}"),
            FixtureFailureKind::Classify(e) => write!(f, "classify error: {e}"),
            FixtureFailureKind::Mismatch { expected, actual } => write!(
                f,
                "output mismatch\n  expected link_tags = {:?}\n  actual   link_tags = {:?}\n  expected info_tags = {:?}\n  actual   info_tags = {:?}\n  expected warnings  = {:?}\n  actual   warnings  = {:?}",
                expected.link_tags,
                actual.link_tags,
                expected.info_tags,
                actual.info_tags,
                expected.warnings,
                actual.warnings,
            ),
        }
    }
}

/// Discover and parse every `fixtures/*.toml` for a tracker.
///
/// Sort order is by path so output is deterministic. Returns `Err` on the
/// first unparseable file so the caller can surface it; per-fixture
/// runtime mismatches are reported through [`FixtureFailure`] instead.
pub fn discover(tracker_dir: &Path) -> Result<Vec<Fixture>, FixtureFailure> {
    let fixtures_dir = tracker_dir.join("fixtures");
    if !fixtures_dir.exists() {
        return Ok(Vec::new());
    }

    let mut paths: Vec<PathBuf> = match fs::read_dir(&fixtures_dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
                    && p.extension().and_then(|x| x.to_str()) == Some("toml")
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| !n.starts_with('.'))
                        .unwrap_or(false)
            })
            .collect(),
        Err(e) => {
            return Err(FixtureFailure {
                tracker: tracker_dir
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("?")
                    .to_string(),
                fixture: fixtures_dir,
                kind: FixtureFailureKind::Io(e),
            });
        }
    };
    paths.sort();

    let mut out = Vec::with_capacity(paths.len());
    for p in paths {
        let txt = fs::read_to_string(&p).map_err(|e| FixtureFailure {
            tracker: String::new(),
            fixture: p.clone(),
            kind: FixtureFailureKind::Io(e),
        })?;
        let parsed: FixtureFile = toml::from_str(&txt).map_err(|e| FixtureFailure {
            tracker: String::new(),
            fixture: p.clone(),
            kind: FixtureFailureKind::Parse(e),
        })?;
        out.push(Fixture {
            path: p,
            input: toml_to_json(parsed.input),
            expected: parsed.expected_output,
        });
    }
    Ok(out)
}

/// Convert a `toml::Value` to a `serde_json::Value`. We do this so the
/// input flows through the same JSON pipeline the transports use.
fn toml_to_json(v: toml::Value) -> Json {
    match v {
        toml::Value::String(s) => Json::String(s),
        toml::Value::Integer(i) => Json::Number(i.into()),
        toml::Value::Float(f) => serde_json::Number::from_f64(f)
            .map(Json::Number)
            .unwrap_or(Json::Null),
        toml::Value::Boolean(b) => Json::Bool(b),
        toml::Value::Datetime(d) => Json::String(d.to_string()),
        toml::Value::Array(a) => Json::Array(a.into_iter().map(toml_to_json).collect()),
        toml::Value::Table(t) => {
            Json::Object(t.into_iter().map(|(k, v)| (k, toml_to_json(v))).collect())
        }
    }
}

/// Run every fixture for one tracker. Returns the count of fixtures
/// executed and the list of failures.
pub fn run_tracker(engine: &Engine, name: &str, tracker: &Tracker) -> (usize, Vec<FixtureFailure>) {
    let fixtures = match discover(&tracker.dir) {
        Ok(f) => f,
        Err(mut e) => {
            if e.tracker.is_empty() {
                e.tracker = name.to_string();
            }
            return (0, vec![e]);
        }
    };

    let total = fixtures.len();
    let mut failures = Vec::new();
    for fx in fixtures {
        if let Err(kind) = run_one(engine, tracker, &fx) {
            failures.push(FixtureFailure {
                tracker: name.to_string(),
                fixture: fx.path,
                kind,
            });
        }
    }
    (total, failures)
}

fn run_one(engine: &Engine, tracker: &Tracker, fx: &Fixture) -> Result<(), FixtureFailureKind> {
    let input = marshal_input(&tracker.manifest, &fx.input).map_err(FixtureFailureKind::Input)?;
    let out = run_classify(
        engine,
        &tracker.script,
        input,
        &tracker.manifest.canonical_category,
    )
    .map_err(FixtureFailureKind::Classify)?;

    if out.link_tags == fx.expected.link_tags
        && out.info_tags == fx.expected.info_tags
        && out.warnings == fx.expected.warnings
    {
        Ok(())
    } else {
        Err(FixtureFailureKind::Mismatch {
            expected: ExpectedOutput {
                link_tags: fx.expected.link_tags.clone(),
                info_tags: fx.expected.info_tags.clone(),
                warnings: fx.expected.warnings.clone(),
            },
            actual: out,
        })
    }
}

/// Run fixtures across the whole registry, optionally filtered to a
/// single tracker name. Returns `(total_run, failures)`. Caller prints
/// the failures and exits non-zero if any.
pub fn run_all(
    engine: &Engine,
    registry: &Registry,
    only: Option<&str>,
) -> (usize, Vec<FixtureFailure>) {
    let mut total = 0;
    let mut failures = Vec::new();
    for (name, tracker) in registry.iter() {
        if let Some(filter) = only {
            if name != filter {
                continue;
            }
        }
        let (n, f) = run_tracker(engine, name, tracker);
        total += n;
        failures.extend(f);
    }
    (total, failures)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scripting::registry::load_dir;
    use crate::scripting::sandbox::{build_engine, SandboxLimits};
    use std::io::Write;

    fn engine() -> Engine {
        build_engine(&SandboxLimits::default())
    }

    fn tmp_root() -> TempDir {
        TempDir::new("tql-fixtures-test")
    }

    fn write_file(p: &Path, contents: &str) {
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut f = fs::File::create(p).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
    }

    const MANIFEST: &str = r#"
name = "mam"
canonical_category = "myanonamouse.net"
description = "MAM"

[[input]]
name = "url"
type = "string"
required = true

[[input]]
name = "categories"
type = "array<string>"
required = true
min_items = 1

[[input]]
name = "author"
type = "string"
required = false
"#;

    const SCRIPT: &str = r#"
fn classify(input) {
    let tags = [];
    for cat in input.categories {
        if input.author != () {
            tags.push("link:" + cat + "/" + input.author);
        } else {
            tags.push("link:" + cat);
        }
    }
    #{ link_tags: tags, info_tags: [], warnings: [] }
}
"#;

    fn write_basic_tracker(root: &Path) -> PathBuf {
        let dir = root.join("mam");
        write_file(&dir.join("manifest.toml"), MANIFEST);
        write_file(&dir.join("classify.rhai"), SCRIPT);
        dir
    }

    #[test]
    fn discover_no_fixtures_dir_returns_empty() {
        let tmp = tmp_root();
        let dir = write_basic_tracker(tmp.path());
        let fxs = discover(&dir).unwrap();
        assert!(fxs.is_empty());
    }

    #[test]
    fn discover_sorts_and_filters_to_toml() {
        let tmp = tmp_root();
        let dir = write_basic_tracker(tmp.path());
        write_file(
            &dir.join("fixtures/b.toml"),
            r#"
[input]
url = "u"
categories = ["c"]
[expected_output]
link_tags = ["link:c"]
"#,
        );
        write_file(
            &dir.join("fixtures/a.toml"),
            r#"
[input]
url = "u"
categories = ["x"]
[expected_output]
link_tags = ["link:x"]
"#,
        );
        // Non-TOML and hidden files: ignored.
        write_file(&dir.join("fixtures/README.md"), "# nope");
        write_file(&dir.join("fixtures/.hidden.toml"), "garbage");

        let fxs = discover(&dir).unwrap();
        assert_eq!(fxs.len(), 2);
        assert!(fxs[0].path.ends_with("a.toml"));
        assert!(fxs[1].path.ends_with("b.toml"));
    }

    #[test]
    fn run_tracker_happy_path() {
        let tmp = tmp_root();
        let dir = write_basic_tracker(tmp.path());
        write_file(
            &dir.join("fixtures/basic.toml"),
            r#"
[input]
url = "https://t/1"
categories = ["Computer/Internet"]
author = "Hamza"

[expected_output]
link_tags = ["link:Computer/Internet/Hamza"]
info_tags = []
warnings = []
"#,
        );
        let report = load_dir(tmp.path(), &engine()).unwrap();
        assert!(report.ok());
        let (total, failures) = run_all(&engine(), &report.registry, None);
        assert_eq!(total, 1);
        assert!(failures.is_empty(), "{failures:?}");
    }

    #[test]
    fn run_tracker_reports_mismatch() {
        let tmp = tmp_root();
        let dir = write_basic_tracker(tmp.path());
        write_file(
            &dir.join("fixtures/wrong.toml"),
            r#"
[input]
url = "u"
categories = ["c"]
[expected_output]
link_tags = ["link:not-what-script-produces"]
"#,
        );
        let report = load_dir(tmp.path(), &engine()).unwrap();
        let (total, failures) = run_all(&engine(), &report.registry, None);
        assert_eq!(total, 1);
        assert_eq!(failures.len(), 1);
        assert!(matches!(
            failures[0].kind,
            FixtureFailureKind::Mismatch { .. }
        ));
    }

    #[test]
    fn run_tracker_reports_parse_error() {
        let tmp = tmp_root();
        let dir = write_basic_tracker(tmp.path());
        write_file(&dir.join("fixtures/broken.toml"), "this is = = not toml");
        let report = load_dir(tmp.path(), &engine()).unwrap();
        let (_total, failures) = run_all(&engine(), &report.registry, None);
        assert_eq!(failures.len(), 1);
        assert!(matches!(failures[0].kind, FixtureFailureKind::Parse(_)));
    }

    #[test]
    fn run_tracker_reports_input_validation_error() {
        let tmp = tmp_root();
        let dir = write_basic_tracker(tmp.path());
        write_file(
            &dir.join("fixtures/bad-input.toml"),
            r#"
[input]
url = "u"
categories = []
[expected_output]
link_tags = []
"#,
        );
        let report = load_dir(tmp.path(), &engine()).unwrap();
        let (_total, failures) = run_all(&engine(), &report.registry, None);
        assert_eq!(failures.len(), 1);
        assert!(matches!(failures[0].kind, FixtureFailureKind::Input(_)));
    }

    #[test]
    fn run_all_filter_limits_to_one_tracker() {
        let tmp = tmp_root();
        write_basic_tracker(tmp.path());

        // Second tracker; both have fixtures.
        let other_manifest = MANIFEST
            .replace("\"mam\"", "\"other\"")
            .replace("myanonamouse.net", "other.example");
        let other_dir = tmp.path().join("other");
        write_file(&other_dir.join("manifest.toml"), &other_manifest);
        write_file(&other_dir.join("classify.rhai"), SCRIPT);
        write_file(
            &other_dir.join("fixtures/x.toml"),
            r#"
[input]
url = "u"
categories = ["a"]
[expected_output]
link_tags = ["link:a"]
"#,
        );
        write_file(
            &tmp.path().join("mam/fixtures/x.toml"),
            r#"
[input]
url = "u"
categories = ["a"]
[expected_output]
link_tags = ["link:a"]
"#,
        );

        let report = load_dir(tmp.path(), &engine()).unwrap();
        assert_eq!(report.registry.len(), 2);

        let (total_all, _) = run_all(&engine(), &report.registry, None);
        assert_eq!(total_all, 2);

        let (total_one, failures) = run_all(&engine(), &report.registry, Some("mam"));
        assert_eq!(total_one, 1);
        assert!(failures.is_empty());

        let (total_none, _) = run_all(&engine(), &report.registry, Some("nope"));
        assert_eq!(total_none, 0);
    }

    // ---- tiny tempdir helper, mirrors registry::tests::tempdir_lite --------
    struct TempDir {
        path: PathBuf,
    }
    impl TempDir {
        fn new(prefix: &str) -> Self {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static COUNTER: AtomicUsize = AtomicUsize::new(0);
            let pid = std::process::id();
            let n = COUNTER.fetch_add(1, Ordering::SeqCst);
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0);
            let path = std::env::temp_dir().join(format!("{prefix}-{pid}-{nanos}-{n}"));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }
        fn path(&self) -> &Path {
            &self.path
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
