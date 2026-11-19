//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL sequence declarations, names, and ownership binding.

/// Fixed public fields of a sequence relation, in physical attribute order.
pub const ROW_COLUMN_NAMES: [&str; 3] = ["last_value", "log_cnt", "is_called"];

pub mod actions;
pub mod declaration;
pub mod definition;
pub mod implicit;
pub mod ownership;

pub mod implicit_ownership;

pub mod lifecycle;
pub mod names;
pub mod native;

pub mod dependencies;

pub mod owner_migration;
