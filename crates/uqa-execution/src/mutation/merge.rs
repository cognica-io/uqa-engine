//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! MERGE pairing spill, statement events, and physical result projection.
pub mod codec;
pub mod returning;
pub mod statement_events;

mod actions;
pub mod analysis;
mod model;
pub mod table;

pub mod views;
