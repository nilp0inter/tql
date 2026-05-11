//! `tql` — Tracker-Qualified Layout.
//!
//! Single binary, multi-mode. See DESIGN.md for the contract.

use clap::{Parser, Subcommand};

mod cmd;
mod config;
mod fetch;
mod linking;
mod media;
mod notify;
mod paths;
mod pidfile;
mod qbit;
mod scripting;
mod sidecar;
#[cfg(test)]
mod test_http;
mod torrent;

#[derive(Parser, Debug)]
#[command(
    name = "tql",
    version,
    about = "Tracker-Qualified Layout for qBittorrent"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run the MCP server (stdio or HTTP).
    Mcp(cmd::mcp::Args),
    /// Run the REST API server.
    Api(cmd::api::Args),
    /// Per-tracker CLI add. `tql cli` with no tracker lists registered trackers.
    Cli(cmd::cli::Args),
    /// qBittorrent "torrent finished" hook.
    PostProcess(cmd::post_process::Args),
    /// Reconcile filesystem with current qBittorrent tag state.
    Reconcile(cmd::reconcile::Args),
    /// Drain the notification spool and dispatch to configured backends.
    NotifyFlush(cmd::notify_flush::Args),
    /// Manual link tag operations.
    Link {
        #[command(subcommand)]
        action: LinkAction,
    },
    /// Sidecar inspection.
    Sidecar {
        #[command(subcommand)]
        action: SidecarAction,
    },
    /// Validate installation, config, trackers, qBittorrent reachability.
    Doctor(cmd::doctor::Args),
    /// Run all tracker fixtures (or one tracker's).
    Test(cmd::test::Args),
    /// Re-read trackers/ in a running mcp/api server (or no-op).
    Reload(cmd::reload::Args),
}

#[derive(Subcommand, Debug)]
enum LinkAction {
    Add(cmd::link_add::Args),
    Remove(cmd::link_remove::Args),
}

#[derive(Subcommand, Debug)]
enum SidecarAction {
    Show(cmd::sidecar_show::Args),
    /// List every sidecar with a brief summary.
    List(cmd::sidecar_list::Args),
    /// Remove sidecars whose torrents are no longer in qBittorrent.
    Gc(cmd::sidecar_gc::Args),
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Mcp(a) => cmd::mcp::run(a),
        Command::Api(a) => cmd::api::run(a),
        Command::Cli(a) => cmd::cli::run(a),
        Command::PostProcess(a) => cmd::post_process::run(a),
        Command::Reconcile(a) => cmd::reconcile::run(a),
        Command::NotifyFlush(a) => cmd::notify_flush::run(a),
        Command::Link { action } => match action {
            LinkAction::Add(a) => cmd::link_add::run(a),
            LinkAction::Remove(a) => cmd::link_remove::run(a),
        },
        Command::Sidecar { action } => match action {
            SidecarAction::Show(a) => cmd::sidecar_show::run(a),
            SidecarAction::List(a) => cmd::sidecar_list::run(a),
            SidecarAction::Gc(a) => cmd::sidecar_gc::run(a),
        },
        Command::Doctor(a) => cmd::doctor::run(a),
        Command::Test(a) => cmd::test::run(a),
        Command::Reload(a) => cmd::reload::run(a),
    };
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(code) => std::process::ExitCode::from(code),
    }
}
