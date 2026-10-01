//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The greatest document identity a statement supplies to each table it writes.

use std::collections::BTreeMap;

use uqa_core::DocId;
use uqa_sql::SQLError;

use crate::mutation::{prepared::PreparedInsertConflict, publication::MutationStorage};

/// Every row written with a supplied identity observes that identity, which raises its table's identity watermark to it, and an observation that raises the watermark is a physical commit. A statement that supplies ascending identities would pay one commit for each row. The watermark only ever becomes the greatest identity observed, so the statement observes the greatest one it prepared before it publishes its rows, and each row then finds its own identity covered.
#[derive(Default)]
pub struct SuppliedIdentities {
    greatest: BTreeMap<String, DocId>,
}

impl SuppliedIdentities {
    /// Take note of a prepared row of `table`.
    pub fn note(&mut self, table: &str, prepared: &PreparedInsertConflict) {
        let PreparedInsertConflict::Insert {
            doc_id,
            supplied: true,
        } = prepared
        else {
            return;
        };
        match self.greatest.get_mut(table) {
            Some(greatest) => *greatest = (*greatest).max(*doc_id),
            None => {
                self.greatest.insert(table.to_owned(), *doc_id);
            }
        }
    }

    /// Observe the greatest identity of each table. The caller is about to publish the rows, in a transaction that may write.
    pub fn observe(self, storage: &dyn MutationStorage) -> Result<(), SQLError> {
        for (table, doc_id) in self.greatest {
            storage.observe_document_identity(&table, doc_id)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
