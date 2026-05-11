//! Subcommand implementations. Each module exposes `Args` (clap derive) and `run`.
//!
//! All subcommands are stubs at this stage; see PLAN.md for the implementation order.

pub mod api;
pub mod cli;
pub mod doctor;
pub mod link_add;
pub mod link_remove;
pub mod mcp;
pub mod notify_flush;
pub mod openapi;
pub mod post_process;
pub mod reconcile;
pub mod reload;
pub mod sidecar_show;
pub mod test;

/// Convenience: emit the standard "not yet implemented" message and succeed.
pub(crate) fn unimplemented(name: &str) -> Result<(), u8> {
    eprintln!("tql {name}: not yet implemented (see PLAN.md)");
    Ok(())
}
