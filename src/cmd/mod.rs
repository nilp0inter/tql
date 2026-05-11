//! Subcommand implementations. Each module exposes `Args` (clap derive) and `run`.
//!
//! All subcommands are stubs at this stage; see PLAN.md for the implementation order.

pub mod api;
pub mod cli;
pub mod completions;
pub mod config_init;
pub mod config_show;
pub mod config_validate;
pub mod doctor;
pub mod http_trace;
pub mod link;
pub mod link_add;
pub mod link_remove;
pub mod mcp;
pub mod notify_flush;
pub mod openapi;
pub mod post_process;
pub mod reconcile;
pub mod reload;
pub mod sidecar_gc;
pub mod sidecar_list;
pub mod sidecar_repair;
pub mod sidecar_show;
pub mod sidecar_verify;
pub mod test;
