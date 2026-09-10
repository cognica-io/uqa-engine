//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind SQL schema reference analysis to the active or restored catalog.
use crate::{Engine, StorageBackendError, StorageBackendResult};
use uqa_sql::ast::{ColumnDef, Expr, TableCheck};
pub(super) use uqa_sql::schema::dependencies::regclass::regclass_constant_oid;
impl Engine {
    pub(crate) fn bind_schema_regclass_constants(
        &self,
        expression: &mut Expr,
        loaded: bool,
    ) -> StorageBackendResult<bool> {
        uqa_sql::schema::dependencies::regclass::bind_schema_regclass_constants(
            self, expression, loaded,
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
