//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact physical targets already selected after UPDATE tuple locking and rechecks.

pub(super) use crate::mutation::row_membership::PhysicalRowBuffer as LockedRows;

#[cfg(test)]
mod tests;
