//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Column constraint mutation, key and foreign-key metadata, and identifier allocation.

use super::{
    table_not_found, DocId, Engine, RelationIdentity, SQLError, StorageBackendError,
    StorageBackendResult, TableState,
};

const TABLE_NEXT_ID_METADATA_PREFIX: &str = "uqa.table_next_id.v1:";

pub(crate) fn table_next_id_metadata_key(table: &str) -> String {
    format!("{TABLE_NEXT_ID_METADATA_PREFIX}{table}")
}

pub(crate) use uqa_sql::schema::constraint_metadata::foreign_keys_match_without_object_id;

pub(crate) fn materialize_constraint_metadata(
    relation: &RelationIdentity,
    columns: &mut [uqa_sql::ast::ColumnDef],
    constraints: &mut uqa_sql::ast::TableConstraintSet,
) -> StorageBackendResult<bool> {
    uqa_sql::schema::constraint_metadata::materialize_constraint_metadata(
        relation,
        columns,
        constraints,
        &mut crate::capabilities::allocate_catalog_object_id,
    )
    .map_err(|error| StorageBackendError::Other(error.to_string()))
}

mod engine;
