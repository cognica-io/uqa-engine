//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluate physical tuple keys before borrowing command frames; retain them with their row version.

use super::{
    resource_error, Arc, BudgetedVec, CommandMutationOverlay, CommandStoredDocument, DocId,
    Document, DocumentChanges, DocumentMetadata, KeyKind, RetainedDocumentFields, SQLError,
    StorageBackendResult, StorageReadControl, Value,
};
use crate::catalog::index::physical::PhysicalIndexDefinitions;
use crate::mutation::constraints::index_keys::IndexExpressionContext;

impl CommandStoredDocument {
    /// The expressions run before the caller locks its frame stack, so callbacks can read the original command view.
    pub fn indexed(
        fields: Arc<Document>,
        metadata: DocumentMetadata,
        table: &str,
        indexes: &PhysicalIndexDefinitions,
        expressions: IndexExpressionContext<'_>,
        control: &StorageReadControl,
    ) -> Result<Self, SQLError> {
        let values = indexes.command_expression_values(expressions, table, &fields)?;
        Self::new(fields, metadata, control)
            .map_err(resource_error)?
            .with_index_values(values, control)
            .map_err(resource_error)
    }

    pub(super) fn with_index_values(
        mut self,
        values: Document,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        if !values.is_empty() {
            self.index_values = Some(RetainedDocumentFields::new(Arc::new(values), control)?);
        }
        Ok(self)
    }
}

/// Exact physical tuple matches and the same command view that masks older stored keys. Transaction-private stored rows are already represented by the mutation index reader.
pub struct CommandIndexProbe {
    pub matches: BudgetedVec<DocId>,
    pub changes: DocumentChanges,
}

impl CommandMutationOverlay {
    pub fn column_matches(
        overlays: &mut [Self],
        table: &str,
        fields: &[String],
        values: &[Value],
        control: &StorageReadControl,
    ) -> Result<CommandIndexProbe, SQLError> {
        let matches = Self::matches(overlays, table, fields, values, control)?;
        let changes = Self::changes(overlays, table, DocumentChanges::default(), control)?;
        Ok(CommandIndexProbe { matches, changes })
    }

    pub fn expression_matches(
        overlays: &mut [Self],
        table: &str,
        physical_key: &str,
        values: &[Value],
        control: &StorageReadControl,
    ) -> Result<CommandIndexProbe, SQLError> {
        let matches = Self::matches_keys(
            overlays,
            table,
            &[physical_key.to_owned()],
            &[Value::Row(values.to_vec().into())],
            KeyKind::Expressions,
            control,
        )?;
        let changes = Self::changes(overlays, table, DocumentChanges::default(), control)?;
        Ok(CommandIndexProbe { matches, changes })
    }
}
