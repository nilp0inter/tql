//! `tql cli` — manifest-driven per-tracker subcommands (DESIGN.md §7, §13).
//!
//! With no tracker argument: list registered trackers.
//!
//! With `tql cli <tracker> [--field value ...] <source>`: build a dynamic clap
//! [`Command`] from the tracker's manifest, parse the user's flags, marshal
//! them into a JSON object, and run the tracker's `classify(input)` script
//! to preview the classification.
//!
//! qBittorrent add wiring (fetching the source + POSTing to `/torrents/add`)
//! is deferred to Leg 12b; this leg stops at the classify-and-print step.

use std::path::PathBuf;

use clap::{Arg, ArgAction, ArgMatches, Command, Parser};
use serde_json::{Map as JsonMap, Number, Value as Json};

use crate::config;
use crate::scripting::input::marshal_input;
use crate::scripting::host::run_classify;
use crate::scripting::manifest::{FieldType, InputField, Manifest};
use crate::scripting::registry::{load_dir, Registry};
use crate::scripting::sandbox::{build_engine, SandboxLimits};

/// Per-tracker CLI add. The real `Args` only captures the tracker name and a
/// raw remainder of argv; the manifest-driven parser is built in [`dispatch`].
#[derive(Parser, Debug)]
#[command(disable_help_flag = true)]
pub struct Args {
    /// Tracker name, e.g. `myanonamouse`. Omit to list registered trackers.
    pub tracker: Option<String>,
    /// Remaining arguments forwarded to the per-tracker subcommand.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub rest: Vec<String>,

    /// Explicit config file path; overrides the default search.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
}

pub fn run(args: Args) -> Result<(), u8> {
    let (_path, cfg) = match config::load(args.config.as_deref()) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("cli: {e}");
            return Err(1);
        }
    };

    let engine = build_engine(&SandboxLimits::default());
    let report = match load_dir(&cfg.paths.trackers_root, &engine) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("cli: {e}");
            return Err(1);
        }
    };
    for f in &report.failures {
        eprintln!("cli: load failure: {f}");
    }

    dispatch(args.tracker.as_deref(), &args.rest, &report.registry, &engine)
}

/// Dispatch the registry-loaded part of `run`. Split out so tests can supply
/// their own [`Registry`] without touching the filesystem search order.
pub(crate) fn dispatch(
    tracker: Option<&str>,
    rest: &[String],
    registry: &Registry,
    engine: &rhai::Engine,
) -> Result<(), u8> {
    let Some(name) = tracker else {
        list_trackers(registry);
        return Ok(());
    };

    let Some(tracker) = registry.get(name) else {
        eprintln!("cli: tracker {name:?} not found in registry");
        return Err(1);
    };

    let cmd = build_command(&tracker.manifest);
    // `cmd.get_matches_from` expects argv with program name first.
    let mut argv: Vec<String> = Vec::with_capacity(rest.len() + 1);
    argv.push(format!("tql cli {name}"));
    argv.extend(rest.iter().cloned());

    let matches = match cmd.try_get_matches_from(argv) {
        Ok(m) => m,
        Err(e) => {
            // clap's error already includes formatting; preserve exit code 2
            // for usage errors, 0 for `--help` / `--version` exits.
            let code = if e.use_stderr() { 2 } else { 0 };
            let _ = e.print();
            return if code == 0 { Ok(()) } else { Err(code) };
        }
    };

    let input = match matches_to_json(&tracker.manifest, &matches) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("cli: {e}");
            return Err(2);
        }
    };

    let source = matches
        .get_one::<String>("SOURCE")
        .cloned()
        .expect("clap enforces required SOURCE positional");
    let source_kind = SourceKind::from_str(&source);

    let map = match marshal_input(&tracker.manifest, &input) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("cli: input error: {e}");
            return Err(2);
        }
    };

    let output = match run_classify(
        engine,
        &tracker.script,
        map,
        &tracker.manifest.canonical_category,
    ) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("cli: classify error: {e}");
            return Err(1);
        }
    };

    print_preview(name, &source, source_kind, &input, &output);
    eprintln!("cli: qBittorrent add not yet wired (Leg 12b); classification preview only");
    Ok(())
}

fn list_trackers(registry: &Registry) {
    if registry.is_empty() {
        println!("(no trackers registered)");
        return;
    }
    for (name, t) in registry.iter() {
        println!("{} \t[{}]", name, t.manifest.canonical_category);
        let desc = t.manifest.description.lines().next().unwrap_or("");
        if !desc.is_empty() {
            println!("    {desc}");
        }
    }
}

/// Build a dynamic clap [`Command`] from a tracker manifest.
///
/// Each declared input field becomes a long flag; the torrent source is a
/// required trailing positional `SOURCE`.
pub(crate) fn build_command(manifest: &Manifest) -> Command {
    let mut cmd = Command::new(leak_str(&manifest.name))
        .about(leak_str(&manifest.description))
        .disable_help_subcommand(true);

    for f in &manifest.inputs {
        cmd = cmd.arg(build_arg(f));
    }

    cmd.arg(
        Arg::new("SOURCE")
            .value_name("SOURCE")
            .help("Path, http(s):// URL, or magnet: URI for the .torrent")
            .required(true)
            .num_args(1),
    )
}

fn build_arg(f: &InputField) -> Arg {
    let long: &'static str = leak_str(
        &f.cli_flag
            .clone()
            .unwrap_or_else(|| f.name.replace('_', "-")),
    );
    let id: &'static str = leak_str(&f.name);
    let help: &'static str = leak_str(&f.description);
    let mut a = Arg::new(id).long(long).help(help);

    match &f.field_type {
        FieldType::String => {
            a = a.num_args(1).value_parser(clap::value_parser!(String));
        }
        FieldType::Int => {
            a = a.num_args(1).value_parser(clap::value_parser!(i64));
        }
        FieldType::Bool => {
            a = a.num_args(0).action(ArgAction::SetTrue);
        }
        FieldType::Enum(variants) => {
            a = a
                .num_args(1)
                .value_parser(clap::builder::PossibleValuesParser::new(
                    variants.iter().map(|s| leak_str(s)),
                ));
        }
        FieldType::Array(inner) => {
            a = a.action(ArgAction::Append);
            if let Some(sep) = &f.cli_separator {
                if let Some(c) = sep.chars().next() {
                    a = a.value_delimiter(c);
                }
            }
            a = match inner.as_ref() {
                FieldType::Int => a.num_args(1).value_parser(clap::value_parser!(i64)),
                FieldType::Bool => a.num_args(1).value_parser(clap::value_parser!(bool)),
                FieldType::Enum(variants) => a.num_args(1).value_parser(
                    clap::builder::PossibleValuesParser::new(
                        variants.iter().map(|s| leak_str(s)),
                    ),
                ),
                _ => a.num_args(1).value_parser(clap::value_parser!(String)),
            };
        }
        FieldType::MapStringString => {
            a = a
                .num_args(1)
                .action(ArgAction::Append)
                .value_parser(clap::value_parser!(String))
                .help(leak_str(&format!(
                    "{} (repeatable, KEY=VALUE)",
                    f.description
                )));
        }
    }

    a
}

/// clap's `PossibleValuesParser` wants `&'static str`-shaped variants, but the
/// manifest gives us `String`. Per-invocation leakage is acceptable: a `tql cli`
/// invocation builds at most one dynamic command and then exits.
fn leak_str(s: &str) -> &'static str {
    Box::leak(s.to_owned().into_boxed_str())
}

/// Walk the parsed [`ArgMatches`] and build a JSON object mirroring the
/// manifest's input schema. Missing optional fields are omitted (rather than
/// emitted as `null`) so `marshal_input` can apply manifest defaults.
pub(crate) fn matches_to_json(
    manifest: &Manifest,
    matches: &ArgMatches,
) -> Result<Json, String> {
    let mut obj = JsonMap::new();
    for f in &manifest.inputs {
        match &f.field_type {
            FieldType::String | FieldType::Enum(_) => {
                if let Some(v) = matches.get_one::<String>(&f.name) {
                    obj.insert(f.name.clone(), Json::String(v.clone()));
                }
            }
            FieldType::Int => {
                if let Some(v) = matches.get_one::<i64>(&f.name) {
                    obj.insert(f.name.clone(), Json::Number(Number::from(*v)));
                }
            }
            FieldType::Bool => {
                // SetTrue: only emit when explicitly given; absence = default/omitted.
                if matches.get_flag(&f.name) {
                    obj.insert(f.name.clone(), Json::Bool(true));
                }
            }
            FieldType::Array(inner) => {
                let arr: Option<Vec<Json>> = match inner.as_ref() {
                    FieldType::Int => matches.get_many::<i64>(&f.name).map(|it| {
                        it.map(|n| Json::Number(Number::from(*n))).collect()
                    }),
                    FieldType::Bool => matches
                        .get_many::<bool>(&f.name)
                        .map(|it| it.map(|b| Json::Bool(*b)).collect()),
                    _ => matches.get_many::<String>(&f.name).map(|it| {
                        it.map(|s| Json::String(s.clone())).collect()
                    }),
                };
                if let Some(values) = arr {
                    obj.insert(f.name.clone(), Json::Array(values));
                }
            }
            FieldType::MapStringString => {
                if let Some(values) = matches.get_many::<String>(&f.name) {
                    let mut inner = JsonMap::new();
                    for raw in values {
                        let (k, v) = raw.split_once('=').ok_or_else(|| {
                            format!(
                                "field `{}`: expected KEY=VALUE, got {raw:?}",
                                f.name
                            )
                        })?;
                        inner.insert(k.to_string(), Json::String(v.to_string()));
                    }
                    obj.insert(f.name.clone(), Json::Object(inner));
                }
            }
        }
    }
    Ok(Json::Object(obj))
}

/// Classify the source string by URI prefix (DESIGN.md §13).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceKind {
    Magnet,
    Url,
    File,
}

impl SourceKind {
    fn from_str(s: &str) -> Self {
        if s.starts_with("magnet:") {
            SourceKind::Magnet
        } else if s.starts_with("http://") || s.starts_with("https://") {
            SourceKind::Url
        } else {
            SourceKind::File
        }
    }

    fn label(self) -> &'static str {
        match self {
            SourceKind::Magnet => "magnet",
            SourceKind::Url => "url",
            SourceKind::File => "file",
        }
    }
}

fn print_preview(
    tracker: &str,
    source: &str,
    kind: SourceKind,
    input: &Json,
    output: &crate::scripting::types::ClassifyOutput,
) {
    println!("tracker:      {tracker}");
    println!("source:       {source}  [{}]", kind.label());
    if let Ok(pretty) = serde_json::to_string_pretty(input) {
        println!("input:\n{pretty}");
    }
    println!("link_tags:");
    for t in &output.link_tags {
        println!("  - {t}");
    }
    if !output.info_tags.is_empty() {
        println!("info_tags:");
        for t in &output.info_tags {
            println!("  - {t}");
        }
    }
    if !output.warnings.is_empty() {
        println!("warnings:");
        for w in &output.warnings {
            println!("  - {w}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scripting::manifest;

    fn parse_manifest(toml: &str) -> Manifest {
        manifest::parse(toml).expect("manifest parses")
    }

    #[test]
    fn source_kind_detection() {
        assert_eq!(SourceKind::from_str("magnet:?xt=urn:btih:abc"), SourceKind::Magnet);
        assert_eq!(SourceKind::from_str("https://x/y.torrent"), SourceKind::Url);
        assert_eq!(SourceKind::from_str("http://x/y.torrent"), SourceKind::Url);
        assert_eq!(SourceKind::from_str("/tmp/foo.torrent"), SourceKind::File);
        assert_eq!(SourceKind::from_str("foo.torrent"), SourceKind::File);
    }

    #[test]
    fn build_command_lists_flags_and_source() {
        let m = parse_manifest(
            r#"
name = "demo"
canonical_category = "demo.org"
description = "demo"
[[input]]
name = "url"
type = "string"
required = true
description = "the url"
[[input]]
name = "count"
type = "int"
required = false
description = "count"
[[input]]
name = "tags"
type = "array<string>"
required = false
description = "tags"
"#,
        );
        let cmd = build_command(&m);
        let rendered = cmd.clone().render_long_help().to_string();
        assert!(rendered.contains("--url"));
        assert!(rendered.contains("--count"));
        assert!(rendered.contains("--tags"));
        assert!(rendered.contains("SOURCE"));
    }

    #[test]
    fn missing_source_is_a_usage_error() {
        let m = parse_manifest(
            r#"
name = "demo"
canonical_category = "demo.org"
description = "demo"
[[input]]
name = "url"
type = "string"
required = true
description = "the url"
"#,
        );
        let cmd = build_command(&m);
        let err = cmd.try_get_matches_from(["tql", "--url", "x"]).unwrap_err();
        assert!(err.use_stderr());
    }

    #[test]
    fn matches_to_json_string_int_bool() {
        let m = parse_manifest(
            r#"
name = "demo"
canonical_category = "demo.org"
description = "demo"
[[input]]
name = "url"
type = "string"
required = true
description = "u"
[[input]]
name = "count"
type = "int"
required = false
description = "c"
[[input]]
name = "flag"
type = "bool"
required = false
description = "f"
"#,
        );
        let matches = build_command(&m)
            .try_get_matches_from(["tql", "--url", "https://x", "--count", "42", "--flag", "src.torrent"])
            .unwrap();
        let json = matches_to_json(&m, &matches).unwrap();
        assert_eq!(json["url"], Json::String("https://x".into()));
        assert_eq!(json["count"], Json::Number(Number::from(42)));
        assert_eq!(json["flag"], Json::Bool(true));
    }

    #[test]
    fn matches_to_json_array_repeated_flag() {
        let m = parse_manifest(
            r#"
name = "demo"
canonical_category = "demo.org"
description = "demo"
[[input]]
name = "categories"
type = "array<string>"
required = true
description = "cs"
"#,
        );
        let matches = build_command(&m)
            .try_get_matches_from([
                "tql",
                "--categories", "a",
                "--categories", "b",
                "--categories", "c",
                "src.torrent",
            ])
            .unwrap();
        let json = matches_to_json(&m, &matches).unwrap();
        assert_eq!(
            json["categories"],
            Json::Array(vec![
                Json::String("a".into()),
                Json::String("b".into()),
                Json::String("c".into()),
            ])
        );
    }

    #[test]
    fn matches_to_json_array_cli_separator() {
        let m = parse_manifest(
            r#"
name = "demo"
canonical_category = "demo.org"
description = "demo"
[[input]]
name = "labels"
type = "array<string>"
required = false
cli_separator = ","
description = "ls"
"#,
        );
        let matches = build_command(&m)
            .try_get_matches_from(["tql", "--labels", "a,b,c", "src.torrent"])
            .unwrap();
        let json = matches_to_json(&m, &matches).unwrap();
        assert_eq!(
            json["labels"],
            Json::Array(vec![
                Json::String("a".into()),
                Json::String("b".into()),
                Json::String("c".into()),
            ])
        );
    }

    #[test]
    fn matches_to_json_enum_and_array_enum() {
        let m = parse_manifest(
            r#"
name = "demo"
canonical_category = "demo.org"
description = "demo"
[[input]]
name = "fmt"
type = "enum<FLAC,MP3>"
required = true
description = "f"
[[input]]
name = "medias"
type = "array<enum<CD,Web>>"
required = false
description = "m"
"#,
        );
        let matches = build_command(&m)
            .try_get_matches_from([
                "tql", "--fmt", "FLAC", "--medias", "CD", "--medias", "Web", "src.torrent",
            ])
            .unwrap();
        let json = matches_to_json(&m, &matches).unwrap();
        assert_eq!(json["fmt"], Json::String("FLAC".into()));
        assert_eq!(
            json["medias"],
            Json::Array(vec![Json::String("CD".into()), Json::String("Web".into())])
        );

        // bad enum is rejected by clap
        let err = build_command(&m)
            .try_get_matches_from(["tql", "--fmt", "WAV", "src.torrent"])
            .unwrap_err();
        assert!(err.use_stderr());
    }

    #[test]
    fn matches_to_json_map_kv() {
        let m = parse_manifest(
            r#"
name = "demo"
canonical_category = "demo.org"
description = "demo"
[[input]]
name = "extra"
type = "map<string,string>"
required = false
description = "e"
"#,
        );
        let matches = build_command(&m)
            .try_get_matches_from([
                "tql", "--extra", "key1=val1", "--extra", "key2=val2", "src.torrent",
            ])
            .unwrap();
        let json = matches_to_json(&m, &matches).unwrap();
        let extra = &json["extra"];
        assert_eq!(extra["key1"], Json::String("val1".into()));
        assert_eq!(extra["key2"], Json::String("val2".into()));

        // malformed entry
        let matches = build_command(&m)
            .try_get_matches_from(["tql", "--extra", "nokvhere", "src.torrent"])
            .unwrap();
        let err = matches_to_json(&m, &matches).unwrap_err();
        assert!(err.contains("KEY=VALUE"));
    }

    #[test]
    fn matches_to_json_omits_missing_optionals() {
        let m = parse_manifest(
            r#"
name = "demo"
canonical_category = "demo.org"
description = "demo"
[[input]]
name = "author"
type = "string"
required = false
description = "a"
[[input]]
name = "count"
type = "int"
required = false
description = "c"
"#,
        );
        let matches = build_command(&m)
            .try_get_matches_from(["tql", "src.torrent"])
            .unwrap();
        let json = matches_to_json(&m, &matches).unwrap();
        assert_eq!(json.as_object().unwrap().len(), 0);
    }

    #[test]
    fn cli_flag_override_uses_alternative_long_name() {
        let m = parse_manifest(
            r#"
name = "demo"
canonical_category = "demo.org"
description = "demo"
[[input]]
name = "release_type"
type = "string"
required = false
cli_flag = "release-type"
description = "rt"
"#,
        );
        let matches = build_command(&m)
            .try_get_matches_from(["tql", "--release-type", "Album", "src.torrent"])
            .unwrap();
        let json = matches_to_json(&m, &matches).unwrap();
        assert_eq!(json["release_type"], Json::String("Album".into()));
    }
}
