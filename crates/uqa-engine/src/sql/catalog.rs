//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Adapt the engine's statement capabilities to catalog execution.
use crate::Engine;
use uqa_core::Value;
pub(crate) use uqa_execution::catalog::projection::*;
pub use uqa_sql::catalog::result_type::{postgres_result_type, SQLTypeMetadata};
use uqa_sql::{ColumnType, SQLError};

pub(crate) fn resolve_regclass_oid(engine: &Engine, name: &str) -> Result<Option<i64>, SQLError> {
    uqa_execution::catalog::projection::resolve_regclass_oid(&engine.catalog_execution(), name)
}
pub(crate) fn resolve_regprocedure_oid(engine: &Engine, name: &str) -> Result<Option<i64>, String> {
    uqa_execution::catalog::projection::resolve_regprocedure_oid(&engine.catalog_execution(), name)
}
pub(crate) fn resolve_regnamespace_oid(
    engine: &Engine,
    input: &str,
) -> Result<Option<i64>, SQLError> {
    uqa_execution::catalog::projection::resolve_regnamespace_oid(&engine.catalog_execution(), input)
}
pub(crate) fn resolve_regtype_oid(engine: &Engine, name: &str) -> Result<Option<i64>, SQLError> {
    uqa_execution::catalog::projection::resolve_regtype_oid(&engine.catalog_execution(), name)
}
pub(crate) fn resolve_regrole_oid(engine: &Engine, input: &str) -> Result<Option<i64>, SQLError> {
    uqa_execution::catalog::projection::resolve_regrole_oid(&engine.catalog_execution(), input)
}
pub(crate) fn resolve_regobject_oid(
    engine: &Engine,
    ty: &ColumnType,
    name: &str,
) -> Result<Option<i64>, SQLError> {
    uqa_execution::catalog::projection::resolve_regobject_oid(&engine.catalog_execution(), ty, name)
}
pub(crate) fn resolve_regtype_output(
    engine: &Engine,
    ty: &ColumnType,
    oid: i64,
) -> Result<Option<String>, String> {
    uqa_execution::catalog::projection::resolve_regtype_output(&engine.catalog_execution(), ty, oid)
}
pub(crate) fn resolve_catalog_column_type(engine: &Engine, type_name: &str) -> Option<ColumnType> {
    uqa_execution::catalog::projection::resolve_catalog_column_type(
        &engine.catalog_execution(),
        type_name,
    )
}
pub(crate) fn runtime_constraints(engine: &Engine) -> Result<Vec<RuntimeConstraint>, SQLError> {
    uqa_execution::catalog::projection::runtime_constraints(&engine.catalog_execution())
}
pub(crate) fn resolve_age_label_relation_name(
    engine: &Engine,
    name: &str,
) -> Result<Option<String>, SQLError> {
    uqa_execution::catalog::projection::resolve_age_label_relation_name(
        &engine.catalog_execution(),
        name,
    )
}
pub(crate) fn resolve_catalog_column_type_name(
    engine: &Engine,
    type_name: &str,
) -> Result<uqa_sql::ast::ColumnType, SQLError> {
    uqa_execution::catalog::projection::resolve_catalog_column_type_name(
        &engine.catalog_execution(),
        type_name,
    )
}

impl Engine {
    pub(crate) fn clear_regtype_output_cache(&self) {
        self.runtime.regtype_output_cache.clear();
    }
}

use uqa_execution::catalog::services::{
    CatalogExpressionEvaluation, ViewCatalogCapabilities, ViewCatalogMetadata,
};
use uqa_sql::ast::{Expr, TriggerEvent};
impl CatalogExpressionEvaluation for Engine {
    fn evaluate(&self, expression: &Expr) -> Result<Value, SQLError> {
        crate::sql::scalar::eval_lowered_expression(self, expression, None, &[])
    }
}
impl ViewCatalogCapabilities for Engine {
    fn view_updatability(&self, name: &str) -> Result<ViewCatalogMetadata, SQLError> {
        let metadata =
            uqa_sql::semantics::view_rewrite::view_updatability(self.view_rewrite_context(), name)?;
        Ok(ViewCatalogMetadata {
            catalog: metadata.catalog,
            catalog_columns: metadata.catalog_columns,
            check_option: metadata.check_option,
        })
    }
    fn has_instead_of_trigger(&self, name: &str, event: TriggerEvent) -> Result<bool, SQLError> {
        uqa_sql::semantics::view_rewrite::has_instead_of_trigger(
            self.view_rewrite_context(),
            name,
            event,
        )
    }
}
