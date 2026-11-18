//! Helpers shared by the integration tests (and, from P4.2, the planner benches through `#[path]`).
//!
//! Each test file compiles this module as its own crate and uses a different part of it, so the
//! parts one file leaves unused are not dead code for the suite.
#![allow(dead_code)]

pub mod corpus;
pub mod harness;
