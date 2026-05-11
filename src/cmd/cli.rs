//! `tql cli` — manifest-driven per-tracker subcommands (DESIGN.md §7, §13).
//!
//! With no tracker argument: list registered trackers.
//!
//! With `tql cli <tracker> [--field value ...] <source>`: build a dynamic clap
//! [`Command`] from the tracker's manifest, parse the user's flags, marshal
//! them into a JSON object, run the tracker's `classify(input)` script, and
//! submit the result to qBittorrent's `/api/v2/torrents/add` with the
//! classified category and tags.
//!
//! `--dry-run` stops after classification and prints a human-readable preview
//! without contacting qBittorrent (useful when iterating on a script).

use std::path::PathBuf;

use clap::{Arg, ArgAction, ArgMatches, Command, Parser};
use serde_json::{Map as JsonMap, Number, Value as Json};

use crate::config::{self, Config};
use crate::qbit;
use crate::qbit::types::{AddTorrentParams, TorrentSource};
use crate::scripting::input::marshal_input;
use crate::scripting::host::run_classify;
use crate::scripting::manifest::{FieldType, InputField, Manifest};
use crate::scripting::registry::{load_dir, Registry};
use crate::scripting::sandbox::{build_engine, SandboxLimits};
use crate::scripting::types::ClassifyOutput;

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

    /// Classify-only mode: print a preview, don't contact qBittorrent.
    #[arg(long)]
    pub dry_run: bool,
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

    dispatch(
        args.tracker.as_deref(),
        &args.rest,
        &report.registry,
        &engine,
        &cfg,
        args.dry_run,
    )
}

/// Dispatch the registry-loaded part of `run`. Split out so tests can supply
/// their own [`Registry`] without touching the filesystem search order.
pub(crate) fn dispatch(
    tracker: Option<&str>,
    rest: &[String],
    registry: &Registry,
    engine: &rhai::Engine,
    cfg: &Config,
    dry_run: bool,
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

    if dry_run {
        print_preview(name, &source, source_kind, &input, &output);
        return Ok(());
    }

    let qb = match cfg.qbittorrent.as_ref() {
        Some(q) => q,
        None => {
            eprintln!("cli: config has no [qbittorrent] section (use --dry-run to skip submission)");
            return Err(1);
        }
    };
    let password = match std::env::var(&qb.password_env) {
        Ok(v) => v,
        Err(_) => {
            eprintln!("cli: env var {} is not set", qb.password_env);
            return Err(1);
        }
    };

    let torrent_source = match build_torrent_source(&source, source_kind) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cli: cannot read {source}: {e}");
            return Err(1);
        }
    };
    let file_bytes: Option<Vec<u8>> = match &torrent_source {
        TorrentSource::File { bytes, .. } => Some(bytes.clone()),
        _ => None,
    };

    let params = build_add_params(&tracker.manifest.canonical_category, &output);

    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("cli: tokio runtime: {e}");
            return Err(1);
        }
    };

    let submit = rt.block_on(async {
        let client = qbit::Client::new(&qb.url)?;
        client.login(&qb.username, &password).await?;
        client.add_torrent(torrent_source, &params).await?;
        Ok::<(), qbit::Error>(())
    });
    if let Err(e) = submit {
        eprintln!("cli: qbittorrent: {e}");
        return Err(1);
    }

    let ack = build_ack(
        name,
        &tracker.manifest.canonical_category,
        &source,
        source_kind,
        file_bytes.as_deref(),
        &output,
    );
    match serde_json::to_string_pretty(&ack) {
        Ok(j) => println!("{j}"),
        Err(_) => println!("{{\"ok\":true}}"),
    }
    Ok(())
}

/// Build a [`TorrentSource`] from the user's positional argument.
///
/// File sources are read into memory and uploaded as a multipart part so
/// qBittorrent doesn't need filesystem access to our cwd. Magnet/URL sources
/// pass through verbatim — qBittorrent fetches them itself.
pub(crate) fn build_torrent_source(
    source: &str,
    kind: SourceKind,
) -> Result<TorrentSource, std::io::Error> {
    match kind {
        SourceKind::Magnet | SourceKind::Url => Ok(TorrentSource::Url(source.to_string())),
        SourceKind::File => {
            let bytes = std::fs::read(source)?;
            let filename = std::path::Path::new(source)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("source.torrent")
                .to_string();
            Ok(TorrentSource::File { filename, bytes })
        }
    }
}

pub(crate) fn build_add_params(category: &str, output: &ClassifyOutput) -> AddTorrentParams {
    let mut tags: Vec<String> = Vec::with_capacity(output.link_tags.len() + output.info_tags.len());
    tags.extend(output.link_tags.iter().cloned());
    tags.extend(output.info_tags.iter().cloned());
    AddTorrentParams {
        category: Some(category.to_string()),
        tags,
        ..Default::default()
    }
}

/// Extract the BTIH info hash from a `magnet:?xt=urn:btih:...` URI, if present.
/// qBittorrent's `/torrents/add` response doesn't echo the hash, so the
/// acknowledgment includes it only when it's recoverable from the source.
pub(crate) fn magnet_btih(uri: &str) -> Option<String> {
    let query = uri.split_once('?')?.1;
    for kv in query.split('&') {
        if let Some(v) = kv.strip_prefix("xt=") {
            if let Some(h) = v.strip_prefix("urn:btih:") {
                let h = h.split('&').next().unwrap_or(h);
                return Some(h.to_string());
            }
        }
    }
    None
}

pub(crate) fn build_ack(
    tracker: &str,
    category: &str,
    source: &str,
    kind: SourceKind,
    torrent_bytes: Option<&[u8]>,
    output: &ClassifyOutput,
) -> Json {
    let mut obj = JsonMap::new();
    obj.insert("ok".into(), Json::Bool(true));
    obj.insert("tracker".into(), Json::String(tracker.into()));
    obj.insert("category".into(), Json::String(category.into()));
    let info_hash = match kind {
        SourceKind::Magnet => magnet_btih(source).map(Json::String).unwrap_or(Json::Null),
        SourceKind::File => torrent_bytes
            .and_then(|b| crate::torrent::compute_info_hash(b).ok())
            .map(Json::String)
            .unwrap_or(Json::Null),
        SourceKind::Url => Json::Null,
    };
    obj.insert("info_hash".into(), info_hash);
    obj.insert(
        "link_tags".into(),
        Json::Array(output.link_tags.iter().cloned().map(Json::String).collect()),
    );
    obj.insert(
        "info_tags".into(),
        Json::Array(output.info_tags.iter().cloned().map(Json::String).collect()),
    );
    obj.insert(
        "warnings".into(),
        Json::Array(output.warnings.iter().cloned().map(Json::String).collect()),
    );
    let mut src = JsonMap::new();
    src.insert("kind".into(), Json::String(kind.label().into()));
    src.insert("value".into(), Json::String(source.into()));
    obj.insert("source".into(), Json::Object(src));
    Json::Object(obj)
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
    pub(crate) fn from_str(s: &str) -> Self {
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
    fn magnet_btih_extracts_hash() {
        let h = magnet_btih("magnet:?xt=urn:btih:0123456789abcdef&dn=foo");
        assert_eq!(h.as_deref(), Some("0123456789abcdef"));
        assert_eq!(magnet_btih("magnet:?dn=no-xt"), None);
        assert_eq!(magnet_btih("https://x/y.torrent"), None);
    }

    #[test]
    fn build_torrent_source_passes_through_magnet_and_url() {
        let s = build_torrent_source("magnet:?xt=urn:btih:abc", SourceKind::Magnet).unwrap();
        assert!(matches!(s, TorrentSource::Url(ref u) if u == "magnet:?xt=urn:btih:abc"));
        let s = build_torrent_source("https://x/y.torrent", SourceKind::Url).unwrap();
        assert!(matches!(s, TorrentSource::Url(ref u) if u == "https://x/y.torrent"));
    }

    #[test]
    fn build_torrent_source_reads_file_bytes() {
        let dir = std::env::temp_dir().join(format!("tql-cli-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hello.torrent");
        std::fs::write(&path, b"BENCODED").unwrap();
        let s = build_torrent_source(path.to_str().unwrap(), SourceKind::File).unwrap();
        match s {
            TorrentSource::File { filename, bytes } => {
                assert_eq!(filename, "hello.torrent");
                assert_eq!(bytes, b"BENCODED");
            }
            _ => panic!("expected File variant"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_add_params_concatenates_link_and_info_tags() {
        let out = ClassifyOutput {
            link_tags: vec!["link:A".into(), "link:B".into()],
            info_tags: vec!["FLAC".into()],
            warnings: vec![],
        };
        let p = build_add_params("demo.org", &out);
        assert_eq!(p.category.as_deref(), Some("demo.org"));
        assert_eq!(p.tags, vec!["link:A", "link:B", "FLAC"]);
    }

    #[test]
    fn build_ack_shape_for_magnet_includes_hash() {
        let out = ClassifyOutput {
            link_tags: vec!["link:X".into()],
            info_tags: vec![],
            warnings: vec![],
        };
        let ack = build_ack(
            "demo",
            "demo.org",
            "magnet:?xt=urn:btih:deadbeef",
            SourceKind::Magnet,
            None,
            &out,
        );
        assert_eq!(ack["ok"], Json::Bool(true));
        assert_eq!(ack["tracker"], Json::String("demo".into()));
        assert_eq!(ack["category"], Json::String("demo.org".into()));
        assert_eq!(ack["info_hash"], Json::String("deadbeef".into()));
        assert_eq!(ack["source"]["kind"], Json::String("magnet".into()));
        assert_eq!(ack["link_tags"][0], Json::String("link:X".into()));
    }

    #[test]
    fn build_ack_file_source_has_null_info_hash() {
        let out = ClassifyOutput {
            link_tags: vec![],
            info_tags: vec![],
            warnings: vec![],
        };
        let ack = build_ack(
            "demo",
            "demo.org",
            "/tmp/x.torrent",
            SourceKind::File,
            None,
            &out,
        );
        assert_eq!(ack["info_hash"], Json::Null);
        assert_eq!(ack["source"]["kind"], Json::String("file".into()));
    }

    #[test]
    fn build_ack_file_source_with_bytes_includes_info_hash() {
        let out = ClassifyOutput {
            link_tags: vec![],
            info_tags: vec![],
            warnings: vec![],
        };
        // Tiny synthesized .torrent (matches the format in torrent.rs tests).
        let mut blob = Vec::new();
        blob.push(b'd');
        blob.extend_from_slice(b"8:announce9:udp://x:1");
        blob.extend_from_slice(b"4:info");
        let info = b"d6:lengthi12e4:name3:fooe";
        blob.extend_from_slice(info);
        blob.push(b'e');

        let ack = build_ack(
            "demo",
            "demo.org",
            "/tmp/x.torrent",
            SourceKind::File,
            Some(&blob),
            &out,
        );
        let expected = crate::torrent::compute_info_hash(&blob).unwrap();
        assert_eq!(ack["info_hash"], Json::String(expected));
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
