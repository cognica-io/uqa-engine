//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Unified descriptors for enforced table keys and standalone unique indexes.
use crate::ast::{Expr, IndexKey, TableKeyConstraint};

/// Runtime key enforcement keeps index predicates separate from SQL constraints.
#[derive(Debug, Clone)]
pub struct EnforcedKey {
    pub constraint: TableKeyConstraint,
    pub keys: Vec<IndexKey>,
    pub index: Option<uqa_core::RelationIdentity>,
    pub predicate: Option<Box<Expr>>,
    pub constraint_owned: bool,
}

impl std::ops::Deref for EnforcedKey {
    type Target = TableKeyConstraint;

    fn deref(&self) -> &Self::Target {
        &self.constraint
    }
}

impl From<TableKeyConstraint> for EnforcedKey {
    fn from(constraint: TableKeyConstraint) -> Self {
        Self {
            keys: constraint
                .columns
                .iter()
                .cloned()
                .map(IndexKey::Column)
                .collect(),
            index: None,
            constraint,
            predicate: None,
            constraint_owned: true,
        }
    }
}
