//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical INSERT row preparation, spill transport and trigger completion.
pub mod codec;
mod known_new;
pub mod rows;
pub mod triggers;

pub mod source;
pub mod supplied_identities;

pub mod table;
