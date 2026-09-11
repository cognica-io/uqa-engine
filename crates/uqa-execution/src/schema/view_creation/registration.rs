//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Regular-view creation, source binding, and replacement publication.
use super::{
    context::{ViewCreationContext, ViewCreationTransactions},
    publication, ViewRegistration,
};
use crate::catalog::view::{StoredView, StoredViewKind};
use uqa_core::RelationIdentity;
use uqa_sql::{
    catalog::{
        regrole_dependencies::StoredRegroleConstants,
        view::{create_view_output_columns, named_view_schema},
    },
    plan::{QueryPlan, UnifiedPlan},
    schema::view_creation::{
        replacement_is_view, validate_replacement_schema, view_creation_target,
    },
    SQLError,
};

pub fn register_view(
    transactions: &dyn ViewCreationTransactions,
    name: &str,
    body: uqa_sql::ast::SelectStmt,
) -> Result<(), SQLError> {
    transactions.with_view_creation(Box::new(move |context| {
        let plan = UnifiedPlan::Query(Box::new(QueryPlan::lower_with(body, &|aggregate: &str| {
            context
                .routines
                .has_registered_aggregate_function(aggregate)
        })));
        let UnifiedPlan::Query(plan) = plan else {
            return Err(SQLError::Internal(
                "view lowering produced a non-query plan".into(),
            ));
        };
        register_view_plan_inner(
            context,
            ViewRegistration {
                name,
                column_names: &[],
                plan: *plan,
                or_replace: true,
                persistence: uqa_sql::ast::RelationPersistence::Permanent,
                options: &[],
                params: &[],
            },
        )
    }))
}
pub fn register_view_plan(
    transactions: &dyn ViewCreationTransactions,
    registration: ViewRegistration<'_>,
) -> Result<(), SQLError> {
    transactions.with_view_creation(Box::new(move |context| {
        register_view_plan_inner(context, registration)
    }))
}
fn replacement_view(
    context: &ViewCreationContext<'_>,
    name: &str,
    relation: &RelationIdentity,
    or_replace: bool,
    replacement_schema: &uqa_sql::RowSchema,
) -> Result<Option<StoredView>, SQLError> {
    let kind = context
        .names
        .relation_kind_at(name)
        .map_err(|error| SQLError::Internal(format!("resolve relation `{name}`: {error}")))?;
    if !replacement_is_view(name, kind, or_replace)? {
        return Ok(None);
    }
    let existing = context.views.view(relation).ok_or_else(|| {
        SQLError::Internal(format!(
            "view `{name}` exists in the catalog but has no loaded definition"
        ))
    })?;
    let existing_schema = uqa_sql::semantics::view_rewrite::context::stored_view_schema(
        context.rewrite,
        &existing.rewrite_definition(),
    )?;
    validate_replacement_schema(&existing_schema, replacement_schema)?;
    context.owners.ensure_owner(name, &existing)?;
    Ok(Some(existing))
}
pub(super) fn reject_regrole_constants(
    context: &ViewCreationContext<'_>,
    plan: &mut QueryPlan,
) -> Result<(), SQLError> {
    let mut constants = StoredRegroleConstants::default();
    constants.collect_query_plan(plan);
    constants.reject_with(context.regroles)
}
fn register_view_plan_inner(
    context: &ViewCreationContext<'_>,
    registration: ViewRegistration<'_>,
) -> Result<(), SQLError> {
    let ViewRegistration {
        name,
        column_names,
        mut plan,
        or_replace,
        persistence,
        options,
        params,
    } = registration;
    context
        .catalog
        .synchronize()
        .map_err(|err| SQLError::Internal(format!("refresh view catalog: {err}")))?;
    let uses_temporary_relation = context.bindings.bind_relations(&mut plan)?;
    let (name, persistence) = view_creation_target(
        &context.namespace,
        name,
        persistence,
        uses_temporary_relation,
    )?;
    let relation = RelationIdentity::from_legacy_name(&name)
        .map_err(|err| SQLError::Internal(format!("invalid canonical view name: {err}")))?;
    let query_schema = context.bindings.bind_routines(&mut plan, params)?;
    reject_regrole_constants(context, &mut plan)?;
    let output_columns = create_view_output_columns(&query_schema, column_names)?;
    for (position, column) in output_columns.iter().enumerate() {
        if let Some(ty) = query_schema.column_type(position) {
            uqa_sql::schema::columns::validate_postgres_relation_column_type(column, ty)?;
        }
    }
    let replacement_schema = named_view_schema(&query_schema, &output_columns)?;
    let existing_view =
        replacement_view(context, &name, &relation, or_replace, &replacement_schema)?;
    let object_id = if let Some(existing) = existing_view.as_ref() {
        existing.object_id
    } else {
        context.catalog.allocate_identity().map_err(|error| {
            SQLError::Internal(format!("allocate view `{name}` identity: {error}"))
        })?
    };
    let view = StoredView {
        object_id,
        role_owner: existing_view.as_ref().map_or_else(
            || context.access.current_user_name(),
            |view| view.role_owner.clone(),
        ),
        acl: existing_view.as_ref().and_then(|view| view.acl.clone()),
        column_acls: existing_view
            .as_ref()
            .map_or_else(std::collections::BTreeMap::new, |view| {
                view.column_acls.clone()
            }),
        query: plan,
        output_columns: Some(output_columns),
        persistence,
        options: options.to_vec(),
        kind: StoredViewKind::View,
        materialized_rows: Vec::new(),
        materialized_column_types: Vec::new(),
        populated: true,
    };
    uqa_sql::semantics::view_rewrite::validate_view_definition_check_option(
        context.rewrite,
        &name,
        &view.rewrite_definition(),
    )?;
    publication::publish_regular_view(context.publication, context.changes, relation, view, &name)?;
    Ok(())
}
