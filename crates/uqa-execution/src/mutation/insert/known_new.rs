//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Which prepared inserts are known to create their document, so publication reads no existing row for them.

use std::collections::HashMap;

use uqa_sql::{semantics::constraint_catalog::ConstraintCatalog, SQLError};

use crate::mutation::{errors::dml_storage_error, prepared::PreparedInsertConflict};

/// An insert creates its document when the statement resolves no conflict by rewriting a row and the document identity is either generated or a unique key, whose conflict check has already found no row.
pub(super) struct KnownNewInserts<'a> {
    catalog: &'a dyn ConstraintCatalog,
    id_column: &'a str,
    resolves_conflicts: bool,
    /// Whether the identity column is a unique key of each target table seen so far.
    unique_identity: HashMap<String, bool>,
}

impl<'a> KnownNewInserts<'a> {
    pub(super) fn new(
        catalog: &'a dyn ConstraintCatalog,
        id_column: &'a str,
        resolves_conflicts: bool,
    ) -> Self {
        Self {
            catalog,
            id_column,
            resolves_conflicts,
            unique_identity: HashMap::new(),
        }
    }

    pub(super) fn contains(
        &mut self,
        target_table: &str,
        prepared: &PreparedInsertConflict,
    ) -> Result<bool, SQLError> {
        if self.resolves_conflicts {
            return Ok(false);
        }
        if !matches!(
            prepared,
            PreparedInsertConflict::Insert { supplied: true, .. }
        ) {
            return Ok(true);
        }
        if let Some(unique) = self.unique_identity.get(target_table) {
            return Ok(*unique);
        }
        let unique = self
            .catalog
            .try_unique_columns(target_table)
            .map_err(|error| dml_storage_error("INSERT", error))?
            .iter()
            .any(|column| column == self.id_column);
        self.unique_identity.insert(target_table.to_owned(), unique);
        Ok(unique)
    }
}

#[cfg(test)]
mod tests;
