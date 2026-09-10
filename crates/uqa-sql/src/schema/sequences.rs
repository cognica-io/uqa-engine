//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL sequence declarations, names, and ownership binding.
pub mod actions;
pub mod definition;
pub mod implicit;
pub mod ownership;

pub mod implicit_ownership;

pub mod lifecycle;
pub mod names;

pub mod dependencies;
