// SPDX-License-Identifier: GPL-3.0-or-later
//! Core data model and placement engine for Qfodufk8s.
//!
//! This crate is deliberately free of I/O: everything here is pure data and
//! pure functions, so the scheduler can be tested exhaustively.

pub mod model;
pub mod scheduler;

pub use model::*;
pub use scheduler::{PlacementError, PlacementPolicy, Scored, place};
