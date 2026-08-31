//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed deferred constraint work registered by the mutation protocol.

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeferredForeignKeyCheck {
    pub(crate) constraint: crate::ConstraintIdentity,
    pub(crate) firing_relation: crate::RelationIdentity,
    pub(crate) row: Option<crate::row_locks::RowLockKey>,
}
