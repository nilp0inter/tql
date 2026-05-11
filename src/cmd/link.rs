//! Shared implementation for `tql link add` / `tql link remove`
//! (DESIGN.md §7).
//!
//! Both subcommands take an info hash + a relative library path
//! (without the `link:` prefix), validate it against §5 producer rules,
//! mutate the torrent's tags in qBittorrent, and then run the same
//! diff/apply pipeline as `tql post-process` against the (now updated)
//! torrent. The on-disk side effect (create/remove a link site under
//! `<library_root>/<category>/<rel>/<name>`) comes from that
//! post-process pass — this command is a thin orchestrator.

use std::path::PathBuf;

use crate::cmd::post_process;
use crate::config;
use crate::paths::{self, LINK_TAG_PREFIX};
use crate::qbit;
use crate::qbit::types::{TorrentInfo, TorrentsInfoQuery};

#[derive(Debug, Clone, Copy)]
pub enum Op {
    Add,
    Remove,
}

impl Op {
    fn label(self) -> &'static str {
        match self {
            Op::Add => "link add",
            Op::Remove => "link remove",
        }
    }
}

pub fn run(
    op: Op,
    hash: &str,
    path: &str,
    config_path: Option<&std::path::Path>,
) -> Result<(), u8> {
    let (_, cfg) = match config::load(config_path) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("tql {}: config: {e}", op.label());
            return Err(1);
        }
    };

    // §5 — validate the tag string offline before touching qBittorrent.
    // We can't enforce the StartsWithCategory rule yet (we don't know the
    // canonical category until we've fetched the torrent), so we pass
    // `None` here and re-check after the fetch.
    let tag = format!("{LINK_TAG_PREFIX}{path}");
    if let Err(e) = paths::parse_link_tag(&tag, None) {
        eprintln!("tql {}: invalid path {path:?}: {e:?}", op.label());
        return Err(1);
    }

    let qb = match cfg.qbittorrent.as_ref() {
        Some(q) => q,
        None => {
            eprintln!("tql {}: config has no [qbittorrent] section", op.label());
            return Err(1);
        }
    };
    let password = match std::env::var(&qb.password_env) {
        Ok(p) => p,
        Err(_) => {
            eprintln!("tql {}: env var {} is not set", op.label(), qb.password_env);
            return Err(1);
        }
    };

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("tql {}: tokio runtime: {e}", op.label());
            return Err(1);
        }
    };

    let url = qb.url.clone();
    let username = qb.username.clone();
    let info: TorrentInfo =
        match runtime.block_on(mutate_qbit(op, &url, &username, &password, hash, &tag)) {
            Ok(i) => i,
            Err(msg) => {
                eprintln!("tql {}: {msg}", op.label());
                return Err(1);
            }
        };

    // Re-validate now that we know the canonical category.
    if let Some(cat) = info.category.as_deref() {
        if let Err(e) = paths::parse_link_tag(&tag, Some(cat)) {
            eprintln!(
                "tql {}: path {path:?} not allowed for category {cat:?}: {e:?}",
                op.label()
            );
            return Err(1);
        }
    } else {
        eprintln!(
            "tql {}: torrent {hash} has no category (tracker-qualified layout requires one)",
            op.label()
        );
        return Err(1);
    }

    // Run the same pipeline post-process uses. The tag we mutated is now
    // reflected in `info.tags`; synthesize Args and re-use the shared
    // process_with_cfg core.
    let pp_args = build_pp_args(&info, config_path.map(PathBuf::from));
    match post_process::process_with_cfg(&pp_args, &cfg, post_process::ProcessOpts::default()) {
        post_process::Outcome::Ok { warnings, .. } => {
            for w in &warnings {
                eprintln!("tql {}: warn: {w}", op.label());
            }
            Ok(())
        }
        post_process::Outcome::Planned { .. } => Ok(()),
        post_process::Outcome::Aborted(reason) => {
            eprintln!("tql {}: aborted: {reason}", op.label());
            Err(1)
        }
    }
}

async fn mutate_qbit(
    op: Op,
    url: &str,
    username: &str,
    password: &str,
    hash: &str,
    tag: &str,
) -> Result<TorrentInfo, String> {
    let client = qbit::Client::new(url).map_err(|e| format!("qbit client: {e}"))?;
    client
        .login(username, password)
        .await
        .map_err(|e| format!("qbit login: {e}"))?;

    let hashes = vec![hash.to_string()];
    let tags = vec![tag.to_string()];
    match op {
        Op::Add => client
            .add_tags(&hashes, &tags)
            .await
            .map_err(|e| format!("qbit addTags: {e}"))?,
        Op::Remove => client
            .remove_tags(&hashes, &tags)
            .await
            .map_err(|e| format!("qbit removeTags: {e}"))?,
    }

    let mut infos = client
        .torrents_info(&TorrentsInfoQuery {
            hashes: hashes.clone(),
            ..Default::default()
        })
        .await
        .map_err(|e| format!("qbit torrents_info: {e}"))?;
    if infos.is_empty() {
        return Err(format!("no torrent with hash {hash}"));
    }
    Ok(infos.swap_remove(0))
}

fn build_pp_args(info: &TorrentInfo, config: Option<PathBuf>) -> post_process::Args {
    let content_path = if info.content_path.is_empty() {
        PathBuf::from(&info.save_path).join(&info.name)
    } else {
        PathBuf::from(&info.content_path)
    };
    post_process::Args {
        hash: info.hash.clone(),
        name: info.name.clone(),
        category: info.category.clone().unwrap_or_default(),
        tags: info.tags.join(","),
        content_path: content_path.to_string_lossy().into_owned(),
        save_path: info.save_path.clone(),
        size: info.size,
        config,
    }
}

/// Pure helper: compute the tag list after applying `op` to `current`.
/// Idempotent — adding a present tag or removing an absent one is a no-op.
#[cfg(test)]
pub fn apply_op(op: Op, current: &[String], tag: &str) -> Vec<String> {
    let mut out: Vec<String> = current.iter().cloned().filter(|t| t != tag).collect();
    if matches!(op, Op::Add) {
        out.push(tag.to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_add_is_idempotent() {
        let cur = vec!["link:A".to_string(), "info:x".to_string()];
        let r = apply_op(Op::Add, &cur, "link:A");
        assert_eq!(r, vec!["info:x".to_string(), "link:A".to_string()]);
    }

    #[test]
    fn apply_add_appends_when_absent() {
        let cur = vec!["info:x".to_string()];
        let r = apply_op(Op::Add, &cur, "link:A");
        assert_eq!(r, vec!["info:x".to_string(), "link:A".to_string()]);
    }

    #[test]
    fn apply_remove_drops_tag() {
        let cur = vec!["link:A".to_string(), "info:x".to_string()];
        let r = apply_op(Op::Remove, &cur, "link:A");
        assert_eq!(r, vec!["info:x".to_string()]);
    }

    #[test]
    fn apply_remove_absent_is_noop() {
        let cur = vec!["info:x".to_string()];
        let r = apply_op(Op::Remove, &cur, "link:A");
        assert_eq!(r, vec!["info:x".to_string()]);
    }
}
