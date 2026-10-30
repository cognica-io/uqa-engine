//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execute view mutation rows, rewrite rules, and INSTEAD OF triggers.
use crate::mutation::output::MutationOutput;
use crate::mutation::statement::context::{with_mutation_snapshot, MutationStatementContext};
use crate::mutation::{
    expressions::eval_mutation_expr,
    returning::{
        build_returning_value_row, dml_returning_result, DmlReturningShape,
        ReturningValueProjectionRow,
    },
    rows::join_rows as dml_join_rows,
};
use crate::query::{sources::build_join_spill_with_ctes, CteScope};
use crate::{OwnedPhysicalRow, PhysicalRow, RowSchema};
use std::collections::BTreeSet;
use uqa_core::Value;

pub use uqa_sql::semantics::view_mutation::{target_columns, ViewMutationTarget as ViewDmlTarget};
use uqa_sql::semantics::{
    mutation_qualifiers::validate_dml_expression_qualifiers,
    returning::validate_returning_alias_relations,
};
use uqa_sql::{
    plan::{DeletePlan, InsertPlan, UpdatePlan},
    SQLError, SQLParam, ScalarExpr,
};
use uqa_storage::document_store::Document;

pub type SourceOutputPruning = fn(
    &dyn uqa_sql::semantics::volatility::VolatilityCatalog,
    &dyn uqa_sql::routines::RoutineResolution,
    uqa_sql::plan::MutableQueryPlan,
    &dyn Fn(usize) -> bool,
    usize,
    &dyn Fn(&str) -> bool,
    &uqa_core::memory::ProductionControl<'_>,
) -> Result<uqa_sql::plan::MutableQueryPlan, SQLError>;

mod checks;
mod insert;
mod update_delete;
pub use checks::TriggerViewChecks;
pub use insert::run_view_insert_inner;
pub use update_delete::{run_view_delete_inner, run_view_update_inner};

pub(crate) fn resolve_view_target<S: Clone + 'static>(
    context: &MutationStatementContext<'_, S>,
    name: &str,
) -> Result<uqa_core::memory::Produced<ViewDmlTarget>, SQLError> {
    let retained = context.query.source.ctes.queries.read_control()?;
    let control = uqa_core::memory::ProductionControl::new(
        retained.memory(),
        retained.cancellation(),
        context.query.source.ctes.runtime.cancellation,
    );
    uqa_sql::semantics::view_mutation::resolve_view_target_with_control(
        context.mutation.rules.views,
        name,
        &control,
    )
}

pub fn materialize_view_rows<S: Clone + Send + Sync + 'static>(
    context: &crate::query::statement::context::QueryContext<'_, S>,
    target: &ViewDmlTarget,
    params: &[SQLParam],
    scope: &mut CteScope<S>,
) -> Result<crate::SharedSpill, SQLError> {
    let retained = context.source.ctes.queries.read_control()?;
    let control = uqa_core::memory::ProductionControl::new(
        retained.memory(),
        retained.cancellation(),
        context.source.ctes.runtime.cancellation,
    );
    let privilege_subject = if target.security_invoker {
        scope.privilege_subject()?.clone_with_control(&control)?
    } else {
        control.finish(
            scope
                .catalog_read_view()?
                .view_owner_with_control(target.security.role_owner, &control)?,
            control.empty_reservation(),
        )?
    };
    let (privilege_subject, _subject_memory) = privilege_subject.into_parts();
    let mut privilege_scope = scope.enter_privilege_subject(privilege_subject);
    let result = crate::query::statement::execute_query_plan_output(
        context,
        &target.query,
        params,
        &mut privilege_scope,
        crate::query::statement::consumer::QueryOutputMode::SharedSpill,
    )?;
    if result.columns.len() != target.columns.len() {
        return Err(SQLError::Internal(format!(
            "view `{}` returned {} columns for a {}-column row type",
            target.canonical_name,
            result.columns.len(),
            target.columns.len()
        )));
    }
    let runtime = context.source.relational.runtime;
    let mut columns = uqa_core::memory::ProductionVec::new(control);
    for column in &target.columns {
        columns.push_produced(control.copy_text(column)?)?;
    }
    let schema = RowSchema::new_with_control(columns.finish()?, &control)?;
    let mut rows = crate::SpillBuffer::new(runtime.work_mem_bytes()?);
    for row in crate::query::output::PublicQueryRows::new(result, "view mutation input")? {
        runtime.check_cancelled()?;
        rows.push(crate::Batch::from_physical_rows(
            schema.clone(),
            vec![PhysicalRow::from_values(row?)],
        ))
        .map_err(crate::physical::physical_exec_error)?;
    }
    rows.into_shared(schema)
        .map_err(crate::physical::physical_exec_error)
}

pub fn target_row(
    target: &ViewDmlTarget,
    qualifier: &str,
    values: &[Value],
) -> Result<OwnedPhysicalRow, SQLError> {
    if values.len() != target.columns.len() {
        return Err(SQLError::Internal(
            "view DML row does not match its declared row type".into(),
        ));
    }
    Ok(OwnedPhysicalRow::new(
        RowSchema::with_relation_qualifier(&target.schema, qualifier),
        PhysicalRow::from_values(values.to_vec()),
    ))
}

fn view_document(target: &ViewDmlTarget, values: &[Value]) -> Result<Document, SQLError> {
    if values.len() != target.columns.len() {
        return Err(SQLError::Internal(
            "view rule row does not match its declared row type".into(),
        ));
    }
    Ok(target
        .columns
        .iter()
        .cloned()
        .zip(values.iter().cloned())
        .collect())
}

fn finish_view_dml<S: Clone + 'static>(
    returning: &crate::mutation::returning::ReturningExecutionContext<'_, S>,
    shape: DmlReturningShape<'_, S>,
    returning_rows: crate::mutation::returning::ReturningRows,
    affected: u64,
) -> Result<MutationOutput, SQLError> {
    if shape.returning.is_empty() {
        return Ok(MutationOutput::from_affected(affected));
    }
    dml_returning_result(*returning, shape, returning_rows, affected)
}
