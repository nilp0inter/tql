use clap::Parser;

#[derive(Parser, Debug)]
pub struct Args {
    /// Print intended diff but make no changes.
    #[arg(long)]
    pub dry_run: bool,
    /// Limit to a single torrent.
    #[arg(long, value_name = "INFO_HASH")]
    pub torrent: Option<String>,
    /// Limit to a single category (tracker).
    #[arg(long, value_name = "NAME")]
    pub category: Option<String>,
}

pub fn run(_args: Args) -> Result<(), u8> {
    super::unimplemented("reconcile")
}
