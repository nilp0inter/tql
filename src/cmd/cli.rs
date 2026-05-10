use clap::Parser;

/// Per-tracker CLI add. The real implementation builds subcommands dynamically
/// from registered tracker manifests; for now this is a stub that captures
/// arbitrary trailing args.
#[derive(Parser, Debug)]
pub struct Args {
    /// Tracker name, e.g. `myanonamouse`. Omit to list registered trackers.
    pub tracker: Option<String>,
    /// Remaining arguments passed to the per-tracker subcommand.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub rest: Vec<String>,
}

pub fn run(_args: Args) -> Result<(), u8> {
    super::unimplemented("cli")
}
