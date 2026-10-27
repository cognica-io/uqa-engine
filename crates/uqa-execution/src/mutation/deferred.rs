//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed deferred constraint work registered by the mutation protocol.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeferredForeignKeyCheck {
    /// The constraint whose deferral governs the check and whose name its violation reports: the foreign key, or the constraint it derives on the referenced partition whose change fired the check.
    pub constraint: uqa_sql::catalog::constraints::ConstraintIdentity,
    pub firing_relation: uqa_core::RelationIdentity,
    pub row: Option<crate::row_locks::RowLockKey>,
    /// The original referenced key that fired this check. Child-row checks and
    /// relation-only constraint events have no retained parent key.
    pub referenced_values: Option<std::sync::Arc<[uqa_core::Value]>>,
}
