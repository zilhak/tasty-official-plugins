//! This crate has no code of its own. It exists so the workspace depends on the
//! tasty plugin crates at one pinned tasty commit; its tests check that pin.
//!
//! The plugin crates' own tests are not run here: Cargo cannot test a git
//! dependency that is not a workspace member, and tasty CI runs them until the
//! sources move into this repository.

#![forbid(unsafe_code)]
