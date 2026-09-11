//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL statement execution, transaction boundaries, and batch scheduling.

pub mod batch;
pub mod compiled;
pub mod cursor;
pub mod portal;
pub mod transactions;

pub mod context;
pub mod plan_executor;

pub mod validation;
