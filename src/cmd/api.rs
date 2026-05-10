use clap::Parser;

#[derive(Parser, Debug)]
pub struct Args {
    /// Override the configured listen address.
    #[arg(long, value_name = "ADDR")]
    pub addr: Option<String>,
}

pub fn run(_args: Args) -> Result<(), u8> {
    super::unimplemented("api")
}
