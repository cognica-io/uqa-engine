//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind SQL schema reference analysis to the active or restored catalog.
use crate::{Engine, StorageBackendError, StorageBackendResult};
use uqa_sql::ast::{ColumnDef, TableCheck};
impl Engine {
    pub(in crate::table_storage) fn bind_legacy_default_sequence_constants(
        &self,
        columns: &mut [ColumnDef],
    ) -> StorageBackendResult<bool> {
        uqa_sql::schema::dependencies::registration::restore_legacy_default_sequence_inputs(
            &self.schema_dependency_binding_context(),
            self,
            columns,
        )
        .map_err(StorageBackendError::Other)
    }

    pub(in crate::table_storage) fn bind_table_schema_regclass_constants(
        &self,
        columns: &mut [ColumnDef],
        checks: &mut [TableCheck],
        loaded: bool,
    ) -> StorageBackendResult<bool> {
        uqa_sql::schema::dependencies::regclass::bind_table_schema_regclass_constants(
            self, columns, checks, loaded,
        )
        .map_err(StorageBackendError::Other)
    }
}
