use clap::Parser;

#[derive(Parser, Debug)]
pub struct Args {
    pub hash: String,
}

pub fn run(_args: Args) -> Result<(), u8> {
    super::unimplemented("sidecar show")
}
