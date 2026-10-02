//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read-only access retained by physical table sources.

use parking_lot::RwLockReadGuard;
use uqa_core::{DocId, Value};
use uqa_sql::ast::ColumnDef;
use uqa_storage::document_store::DocumentStore;

/// A direct table read retains the same identity-checked access-share lock as a SQL source. The caller supplies its selected catalog and data handle; binding and wait revalidation stay in execution.
pub fn bind_direct_table_read<T>(
    session: &dyn crate::row_locks::binding::RelationLockSession,
    resolve: impl FnMut() -> Result<
        Option<crate::row_locks::binding::RelationBinding<T>>,
        uqa_sql::SQLError,
    >,
) -> Result<Option<crate::row_locks::binding::RelationBinding<T>>, uqa_sql::SQLError> {
    crate::row_locks::binding::bind_relation(
        session,
        crate::row_locks::RelationLockMode::AccessShare,
        false,
        resolve,
        |_| Ok(()),
    )
}

/// Only column definitions, shared document reads and the stored values that indexes hold are exposed. A scan cannot mutate catalog state, obtain an engine, or acquire a storage write guard.
pub trait TableRead: Send + Sync {
    fn column_definitions(&self) -> Vec<ColumnDef>;
    fn read_documents(&self) -> RwLockReadGuard<'_, Box<dyn DocumentStore>>;
    /// Whether the table's single integer primary key, if it has one, names its rows' identities: every row whose key lies below `KEY_IDENTITY_LIMIT` has the identity equal to its key, and no other row has an identity below the limit. A table that does not keep this resolves its keys through the key's index.
    fn maps_integer_keys(&self) -> bool;

    /// Visit `fields` of the rows `ids` from index entries alone, in `ids` order and as a document store's point projection reports them: each row, whether it exists, and its values when it does, while `visitor` returns true. Returns the rows visited, or `None` without visiting when the indexes do not hold every field. `visitor` runs while the index entries are lent and must not call back into the engine.
    fn for_each_indexed_fields(
        &self,
        ids: &[DocId],
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, bool, &[&Value]) -> bool,
    ) -> Option<usize> {
        let _ = (ids, fields, visitor);
        None
    }
}

/// The chosen transaction snapshot supplies a table generation and the command's row overlay separately.
pub trait QueryTableAccess: Sync {
    fn serializable_read(
        &self,
        name: &str,
    ) -> Result<Option<crate::serializable::SerializableRelationRead>, uqa_sql::SQLError>;

    fn table(&self, name: &str) -> Result<std::sync::Arc<dyn TableRead>, uqa_sql::SQLError>;

    /// Whether the indexes of `table`, the handle [`Self::table`] returned for `name`, hold `fields` for every row, loading or building what the catalog provides. An index-only read then projects those fields without reading a document.
    fn index_holds_fields(
        &self,
        name: &str,
        table: &std::sync::Arc<dyn TableRead>,
        fields: &[String],
    ) -> Result<bool, uqa_sql::SQLError> {
        let _ = (name, table, fields);
        Ok(false)
    }
    fn command_overlay_changes(
        &self,
        name: &str,
    ) -> Result<Option<super::document_changes::DocumentChanges>, uqa_sql::SQLError>;
    fn table_doc_count(&self, name: &str) -> Result<u64, uqa_sql::SQLError>;
}
