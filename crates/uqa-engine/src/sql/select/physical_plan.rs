//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Projection, ordering, filtering, and relational physical-operator assembly.

mod ordering;
mod projection;

pub(in crate::sql) use projection::physical_exec_error;
