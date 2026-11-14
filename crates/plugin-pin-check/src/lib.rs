//! This crate has no code of its own and is not published. Its tests check that the build
//! list matches the plugin workspace members, that every tasty package in Cargo.lock comes
//! from the one pinned commit, that the egui family locks the tasty versions, and that the
//! plugin manifest versions and ids match their crates; see tests/pin.rs.

#![forbid(unsafe_code)]
