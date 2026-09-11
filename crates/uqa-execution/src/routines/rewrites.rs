//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Select changed routine definitions and publish their freshly compiled bodies in catalog order.

use super::{
    catalog::{RoutineRegistryPublication, RoutineRegistryState},
    compilation::{compile_persisted_sql_function, StoredRoutineCompilationContext},
};
use crate::schema::namespaces::NamespaceCatalogChanges;
use std::sync::Arc;
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::CreateFunction,
    routines::{
        lifecycle::rewrites::{self as analysis},
        merge_columns::statement_has_removed_merge_target,
        SQLUserFunction,
    },
    SQLError,
};

pub struct RoutineRewriteContext<'a> {
    pub registry: &'a dyn RoutineRegistryState,
    pub publication: &'a dyn RoutineRegistryPublication,
    pub compilation: StoredRoutineCompilationContext<'a>,
    pub columns: uqa_sql::binding::stored_columns::StoredColumnBindingContext<'a>,
    pub changes: &'a dyn NamespaceCatalogChanges,
}

pub fn rewrite_routine_relation_references(
    context: &RoutineRewriteContext<'_>,
    from: &RelationIdentity,
    to: &RelationIdentity,
) -> Result<(), SQLError> {
    rewrite_stored_routine_bodies(context, |statement| {
        uqa_sql::catalog::events::renames::rewrite_stored_statement_relation(statement, from, to)
    })
}

pub fn rewrite_routine_column_references(
    context: &RoutineRewriteContext<'_>,
    relation: &RelationIdentity,
    from: &str,
    to: &str,
) -> Result<(), SQLError> {
    rewrite_stored_routine_bodies(context, |statement| {
        uqa_sql::binding::stored_columns::rewrite_stored_statement_column(
            context.columns,
            statement,
            relation,
            from,
            to,
        )
    })
}

pub fn rewrite_stored_routine_bodies(
    context: &RoutineRewriteContext<'_>,
    rewrite: impl FnMut(&mut uqa_sql::ast::Statement) -> Result<bool, SQLError>,
) -> Result<(), SQLError> {
    let registry = context.registry.routine_snapshot();
    let definitions = analysis::rewritten_routine_definitions(&registry, rewrite)?;
    publish_stored_routine_body_rewrites(context, definitions)
}

pub fn publish_stored_routine_body_rewrites(
    context: &RoutineRewriteContext<'_>,
    definitions: Vec<CreateFunction>,
) -> Result<(), SQLError> {
    if definitions.is_empty() {
        return Ok(());
    }
    let mut rewritten = context.registry.routine_snapshot();
    for definition in definitions {
        let function = analysis::routine_body_rewrite_target(&mut rewritten, &definition)?;
        let compiled = compile_persisted_sql_function(&context.compilation, &definition)?;
        *function = Arc::new(SQLUserFunction {
            def: definition,
            compiled,
        });
    }
    context
        .publication
        .persist_routine_definitions(&rewritten)?;
    **context.registry.routines_write() = rewritten;
    context.changes.catalog_registry_changed();
    Ok(())
}

pub fn refresh_stored_merge_target_plans(
    context: &RoutineRewriteContext<'_>,
) -> Result<(), SQLError> {
    rewrite_stored_routine_bodies(context, |statement| {
        statement_has_removed_merge_target(context.compilation.analysis.merge, statement)
    })
}
