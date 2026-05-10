use clap::Parser;

#[derive(Parser, Debug)]
pub struct Args {
    /// Send a test notification and probe each media server.
    #[arg(long)]
    pub probe: bool,
}

pub fn run(_args: Args) -> Result<(), u8> {
    super::unimplemented("doctor")
}
