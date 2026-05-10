//! Tracker scripting: manifests, Rhai engine, registry.
//!
//! Leg 7 added the manifest parser. Leg 8a wires up the sandboxed Rhai
//! engine and the `classify` runner + output validator. The registry and
//! manifest-typed input marshaling come in later legs.

#![allow(dead_code)]

pub mod host;
pub mod input;
pub mod manifest;
pub mod registry;
pub mod sandbox;
pub mod types;
