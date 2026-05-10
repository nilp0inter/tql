//! Tracker scripting: manifests, Rhai engine, registry.
//!
//! Leg 7 lands the manifest parser only. Rhai engine setup, registry, and
//! the host ↔ script marshaling come in later legs.

#![allow(dead_code)]

pub mod manifest;
