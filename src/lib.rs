//! Core battery reading for Battmon.
//!
//! The library never links GTK, so the sampler daemon (`battmond`) stays small
//! and can be built with `--no-default-features`.

pub mod model;
pub mod store;
pub mod sysfs;
