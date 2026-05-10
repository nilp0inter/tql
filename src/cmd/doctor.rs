use std::path::PathBuf;

use clap::Parser;

use crate::config;

#[derive(Parser, Debug)]
pub struct Args {
    /// Send a test notification and probe each media server.
    #[arg(long)]
    pub probe: bool,

    /// Explicit config file path; overrides the default search.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
}

pub fn run(args: Args) -> Result<(), u8> {
    let (path, cfg) = match config::load(args.config.as_deref()) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("doctor: {e}");
            return Err(1);
        }
    };

    println!("config: {}", path.display());
    println!("  paths.seed_root      = {}", cfg.paths.seed_root.display());
    println!("  paths.library_root   = {}", cfg.paths.library_root.display());
    println!("  paths.trackers_root  = {}", cfg.paths.trackers_root.display());
    println!("  linking.prefer       = {:?}", cfg.linking.prefer);
    let qbit_line = match &cfg.qbittorrent {
        Some(q) => format!("{} as {}", q.url, q.username),
        None => "<unset>".into(),
    };
    println!("  qbittorrent          = {qbit_line}");
    println!("  notify.default       = {:?}", cfg.notify.default);
    println!("  trackers configured  = {}", cfg.trackers.len());

    if args.probe {
        eprintln!("doctor: --probe not yet implemented (see PLAN.md leg 16)");
    }

    eprintln!("doctor: config parsed OK; deeper checks not yet implemented (see PLAN.md leg 16)");
    Ok(())
}
