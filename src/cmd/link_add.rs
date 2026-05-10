use clap::Parser;

#[derive(Parser, Debug)]
pub struct Args {
    pub hash: String,
    /// Relative library path (without the `link:` prefix).
    pub path: String,
}

pub fn run(_args: Args) -> Result<(), u8> {
    super::unimplemented("link add")
}
