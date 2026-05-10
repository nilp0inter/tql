use clap::Parser;

#[derive(Parser, Debug)]
pub struct Args {
    /// Use stdio transport (default).
    #[arg(long, conflicts_with = "http")]
    pub stdio: bool,
    /// Listen on this address with the HTTP transport.
    #[arg(long, value_name = "ADDR")]
    pub http: Option<String>,
}

pub fn run(_args: Args) -> Result<(), u8> {
    super::unimplemented("mcp")
}
