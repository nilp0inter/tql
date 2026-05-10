use clap::Parser;

#[derive(Parser, Debug)]
pub struct Args {
    /// Limit to one tracker.
    pub tracker: Option<String>,
}

pub fn run(_args: Args) -> Result<(), u8> {
    super::unimplemented("test")
}
