//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind index-key analysis to the current catalog scope.
use crate::Engine;
pub(super) use uqa_sql::schema::indexes::keys::{key_names, require_column_key};
use uqa_sql::{
    ast::{ColumnType, CreateIndex},
    SQLError,
};
pub(super) fn prepare_index_keys(
    engine: &Engine,
    statement: &mut CreateIndex,
) -> Result<Vec<ColumnType>, SQLError> {
    let scope = crate::capabilities::query_scope::new_for_catalog_binding(engine);
    let binding = uqa_execution::query::binding::binding_context(&scope)?;
    uqa_sql::schema::indexes::keys::prepare_index_keys(
        &uqa_sql::schema::SchemaBindingContext {
            catalog: engine,
            binding: &binding,
        },
        statement,
    )
}
