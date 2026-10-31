//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrow the same structured qualifier/column hash key without constructing owned identifiers.

use super::ColumnIdentity;
use std::{
    borrow::Borrow,
    hash::{Hash, Hasher},
};

pub(super) trait ColumnKey {
    fn parts(&self) -> (Option<&str>, &str);
}

impl ColumnKey for ColumnIdentity {
    fn parts(&self) -> (Option<&str>, &str) {
        (self.qualifier(), self.column())
    }
}

impl ColumnKey for (Option<&str>, &str) {
    fn parts(&self) -> (Option<&str>, &str) {
        *self
    }
}

impl<'a> Borrow<dyn ColumnKey + 'a> for ColumnIdentity {
    fn borrow(&self) -> &(dyn ColumnKey + 'a) {
        self
    }
}

impl PartialEq for dyn ColumnKey + '_ {
    fn eq(&self, other: &Self) -> bool {
        self.parts() == other.parts()
    }
}

impl Eq for dyn ColumnKey + '_ {}

impl Hash for dyn ColumnKey + '_ {
    fn hash<H: Hasher>(&self, state: &mut H) {
        let (qualifier, column) = self.parts();
        qualifier.hash(state);
        column.hash(state);
    }
}

#[cfg(test)]
mod tests;
