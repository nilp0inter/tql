use clap::Parser;

#[derive(Parser, Debug)]
pub struct Args {}

pub fn run(_args: Args) -> Result<(), u8> {
    super::unimplemented("reload")
}
