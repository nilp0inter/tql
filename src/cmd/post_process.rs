use clap::Parser;

/// qBittorrent hook. Argv mirrors DESIGN.md §8 — long flags only.
#[derive(Parser, Debug)]
pub struct Args {
    #[arg(long, value_name = "INFO_HASH")]
    pub hash: String,
    #[arg(long)]
    pub name: String,
    #[arg(long)]
    pub category: String,
    /// qBittorrent passes tags as a single space- or comma-separated string.
    #[arg(long, default_value = "")]
    pub tags: String,
    #[arg(long, value_name = "PATH")]
    pub content_path: String,
    #[arg(long, value_name = "PATH")]
    pub save_path: String,
    #[arg(long, value_name = "BYTES")]
    pub size: u64,
}

pub fn run(_args: Args) -> Result<(), u8> {
    super::unimplemented("post-process")
}
