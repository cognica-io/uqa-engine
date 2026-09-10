//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed deferred constraint work registered by the mutation protocol.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeferredForeignKeyCheck {
    pub constraint: uqa_sql::catalog::constraints::ConstraintIdentity,
    pub firing_relation: uqa_core::RelationIdentity,
    pub row: Option<crate::row_locks::RowLockKey>,
}
