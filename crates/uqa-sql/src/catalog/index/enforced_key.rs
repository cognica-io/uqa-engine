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
    pub index_catalog: Option<super::IndexCatalogIdentity>,
    /// Ordered parent incarnations used to bind a partition-root arbiter to its local physical index.
    pub index_ancestors: Vec<[u8; 16]>,
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
            index_catalog: None,
            index_ancestors: Vec::new(),
            constraint,
            predicate: None,
            constraint_owned: true,
        }
    }
}

/// Foreign keys may reference only non-partial unique keys composed entirely of ordinary columns.
pub fn referenceable_keys(keys: Vec<EnforcedKey>) -> Vec<TableKeyConstraint> {
    keys.into_iter()
        .filter(|key| key.predicate.is_none() && key.keys.iter().all(|key| key.column().is_some()))
        .map(|key| key.constraint)
        .collect()
}

#[cfg(test)]
mod tests;
