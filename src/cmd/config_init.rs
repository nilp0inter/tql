//! `tql config init` — write a starter `config.toml` to disk (or stdout).
//!
//! `tql config show` answers "what's loaded?", but until you *have* a config,
//! there's nothing to load. New operators end up copying examples out of
//! `DESIGN.md` by hand. This subcommand scaffolds a runnable starter file at
//! the default search location (`$XDG_CONFIG_HOME/tql/config.toml`) with
//! placeholder values keyed to env vars, never secrets, matching the
//! `*_env`-only convention enforced everywhere else.
//!
//! Safety: refuses to overwrite an existing file unless `--force` is set.
//! `--stdout` skips the filesystem entirely (useful inside containers or
//! `tql config init --stdout | sudo tee /etc/tql/config.toml`).
//!
//! No tracker creds, no API keys, no passwords are written — every secret is
//! referenced by env-var name only.

use std::path::{Path, PathBuf};

use clap::Parser;

/// Default starter template. Mirrors the structure in DESIGN.md §11.
pub const TEMPLATE: &str = include_str!("config_init_template.toml");

#[derive(Parser, Debug)]
pub struct Args {
    /// Write to this path instead of the default search location.
    #[arg(long, value_name = "PATH")]
    pub output: Option<PathBuf>,

    /// Overwrite the target file if it already exists.
    #[arg(long)]
    pub force: bool,

    /// Print the template to stdout instead of writing a file.
    #[arg(long, conflicts_with_all = ["output", "force"])]
    pub stdout: bool,
}

pub fn run(args: Args) -> Result<(), u8> {
    if args.stdout {
        print!("{TEMPLATE}");
        return Ok(());
    }
    let target = match args.output {
        Some(p) => p,
        None => match default_target() {
            Some(p) => p,
            None => {
                eprintln!(
                    "config init: cannot determine default target (set $XDG_CONFIG_HOME or $HOME, or pass --output)"
                );
                return Err(1);
            }
        },
    };
    write_template(&target, args.force, &mut std::io::stdout())
}

/// Resolve the default target path: the first writable XDG candidate from
/// `default_search_paths`, falling back to `$XDG_CONFIG_HOME/tql/config.toml`
/// or `$HOME/.config/tql/config.toml`. Returns `None` if neither env var is
/// set (e.g. in a stripped sandbox).
fn default_target() -> Option<PathBuf> {
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(xdg.join("tql").join("config.toml"))
}

pub(crate) fn write_template<W: std::io::Write>(
    target: &Path,
    force: bool,
    out: &mut W,
) -> Result<(), u8> {
    if target.exists() && !force {
        eprintln!(
            "config init: refusing to overwrite existing file: {} (pass --force)",
            target.display()
        );
        return Err(1);
    }
    if let Some(parent) = target.parent() {
        if !parent.as_os_str().is_empty() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                eprintln!("config init: mkdir {}: {e}", parent.display());
                return Err(1);
            }
        }
    }
    if let Err(e) = std::fs::write(target, TEMPLATE) {
        eprintln!("config init: write {}: {e}", target.display());
        return Err(1);
    }
    let _ = writeln!(out, "wrote starter config: {}", target.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tempdir() -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "tql-config-init-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn template_parses_as_loadable_config() {
        // The starter template must round-trip through the real loader,
        // otherwise new operators get a confusing parse error on first run.
        let dir = tempdir();
        let path = dir.join("config.toml");
        std::fs::write(&path, TEMPLATE).unwrap();
        let (loaded_path, cfg) = crate::config::load(Some(&path)).expect("template should parse");
        assert_eq!(loaded_path, path);
        // Sanity: `*_env`-only convention — no plaintext password fields.
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(!json.contains("\"password\":"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn writes_to_target_and_creates_parent_dirs() {
        let dir = tempdir();
        let target = dir.join("nested").join("config.toml");
        let mut buf: Vec<u8> = Vec::new();
        write_template(&target, false, &mut buf).unwrap();
        assert!(target.exists());
        let written = std::fs::read_to_string(&target).unwrap();
        assert_eq!(written, TEMPLATE);
        let stdout = String::from_utf8(buf).unwrap();
        assert!(stdout.contains(&target.display().to_string()));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn refuses_to_overwrite_without_force() {
        let dir = tempdir();
        let target = dir.join("config.toml");
        std::fs::File::create(&target)
            .unwrap()
            .write_all(b"existing")
            .unwrap();
        let mut buf: Vec<u8> = Vec::new();
        let res = write_template(&target, false, &mut buf);
        assert_eq!(res, Err(1));
        // Original content preserved.
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "existing");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn force_overwrites_existing_file() {
        let dir = tempdir();
        let target = dir.join("config.toml");
        std::fs::write(&target, "old").unwrap();
        let mut buf: Vec<u8> = Vec::new();
        write_template(&target, true, &mut buf).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), TEMPLATE);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn template_contains_no_plaintext_secrets() {
        // Belt-and-braces: anything that looks like an actual credential
        // would be a footgun for the operator who follows the example.
        let lower = TEMPLATE.to_lowercase();
        for forbidden in [
            "password = \"",
            "api_key = \"",
            "token = \"",
            "secret = \"",
        ] {
            assert!(
                !lower.contains(forbidden),
                "template contains plaintext secret-looking field: {forbidden}"
            );
        }
        // Required *_env hints are present.
        assert!(TEMPLATE.contains("password_env"));
    }
}
