//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Which prepared inserts are known to create their document, so publication reads no existing row for them.

use std::collections::HashMap;

use uqa_sql::{semantics::constraint_catalog::ConstraintCatalog, SQLError};

use crate::mutation::{
    errors::dml_storage_error, identity::MutationIdentifiers, prepared::PreparedInsertConflict,
    publication::InsertedIdentity,
};

use super::supplied_identities::ObservedIdentities;

/// An insert creates its document when the statement resolves no conflict by rewriting a row and the document identity is either generated or a unique key, whose conflict check has already found no row.
pub(super) struct KnownNewInserts<'a> {
    catalog: &'a dyn ConstraintCatalog,
    id_column: &'a str,
    resolves_conflicts: bool,
    /// Whether the identity column is a unique key of each target table seen so far.
    unique_identity: HashMap<String, bool>,
    /// Whether each target table seen so far generates identities no document of it ever had.
    generates_unused: HashMap<String, bool>,
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
            generates_unused: HashMap::new(),
        }
    }

    /// What the statement knows about the identity of a prepared insert. A new document's identity is unused when its table generated it above every identity in use, or when the statement's observation found the table's watermark below it.
    pub(super) fn identity(
        &mut self,
        target_table: &str,
        prepared: &PreparedInsertConflict,
        observed: &ObservedIdentities,
        identifiers: &dyn MutationIdentifiers,
    ) -> Result<InsertedIdentity, SQLError> {
        if !self.contains(target_table, prepared)? {
            return Ok(InsertedIdentity::Unknown);
        }
        let PreparedInsertConflict::Insert { doc_id, supplied } = prepared else {
            return Ok(InsertedIdentity::Vacant);
        };
        if observed.unused(target_table, *doc_id) {
            return Ok(InsertedIdentity::Unused);
        }
        if *supplied {
            return Ok(InsertedIdentity::Vacant);
        }
        let generates_unused = match self.generates_unused.get(target_table) {
            Some(generates_unused) => *generates_unused,
            None => {
                let generates_unused = identifiers.generates_unused_identities(target_table)?;
                self.generates_unused
                    .insert(target_table.to_owned(), generates_unused);
                generates_unused
            }
        };
        Ok(if generates_unused {
            InsertedIdentity::Unused
        } else {
            InsertedIdentity::Vacant
        })
    }

    fn contains(
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
