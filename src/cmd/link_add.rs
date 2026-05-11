use std::path::PathBuf;

use clap::Parser;

use super::link::{self, Op};

#[derive(Parser, Debug)]
pub struct Args {
    pub hash: String,
    /// Relative library path (without the `link:` prefix).
    pub path: String,
    /// Optional config-file override.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
    /// Emit a single JSON document instead of human-readable output.
    #[arg(long)]
    pub json: bool,
}

pub fn run(args: Args) -> Result<(), u8> {
    link::run(
        Op::Add,
        &args.hash,
        &args.path,
        args.config.as_deref(),
        args.json,
    )
}
