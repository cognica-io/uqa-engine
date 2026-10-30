//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Admitted INSERT query schedules.

mod owned;
pub use owned::OwnedInsertRuleQueries;

#[cfg(test)]
pub(super) mod reference;
