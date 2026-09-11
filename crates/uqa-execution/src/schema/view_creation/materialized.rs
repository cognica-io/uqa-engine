//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Materialized-view source execution and snapshot replacement lifecycle.
use super::{
    context::ViewCreationTransactions, publication, registration::reject_regrole_constants,
    MaterializedViewRegistration,
};
use crate::catalog::view::{StoredView, StoredViewKind};
use uqa_core::RelationIdentity;
use uqa_sql::{catalog::view::create_view_output_columns, SQLError};

fn materialized_rows(
    result: &uqa_sql::SQLResult,
    output_columns: &[String],
) -> Result<Vec<uqa_sql::ResultRow>, SQLError> {
    if result.columns.len() != output_columns.len() {
        return Err(SQLError::Internal(format!(
            "materialized-view query schema width {} changed to {} during execution",
            output_columns.len(),
            result.columns.len()
        )));
    }
    result
        .rows
        .iter()
        .enumerate()
        .map(|(row_index, _)| {
            output_columns
                .iter()
                .enumerate()
                .map(|(column_index, column)| {
                    result
                        .value_at(row_index, column_index)
                        .cloned()
                        .map(|value| (column.clone(), value))
                        .ok_or_else(|| {
                            SQLError::Internal(format!(
                                "materialized-view row {row_index} is missing column {column_index}"
                            ))
                        })
                })
                .collect()
        })
        .collect()
}

pub fn register_materialized_view_plan(
    transactions: &dyn ViewCreationTransactions,
    registration: MaterializedViewRegistration<'_>,
) -> Result<Option<u64>, SQLError> {
    let MaterializedViewRegistration {
        name,
        column_names,
        mut plan,
        if_not_exists,
        with_no_data,
        options,
        params,
    } = registration;
    transactions.with_materialized_view_creation(Box::new(move |context| {
        context.catalog.synchronize().map_err(|error| {
            SQLError::Internal(format!("refresh materialized-view catalog: {error}"))
        })?;
        if context.bindings.bind_relations(&mut plan)? {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "materialized views must not use temporary tables or views".into(),
            });
        }
        let query_schema = context.bindings.bind_routines(&mut plan, params)?;
        reject_regrole_constants(context, &mut plan)?;
        let output_columns = create_view_output_columns(&query_schema, column_names)?;
        for column in &output_columns {
            uqa_sql::schema::columns::validate_postgres_column_name(column)?;
        }
        for (position, column) in output_columns.iter().enumerate() {
            if let Some(ty) = query_schema.column_type(position) {
                uqa_sql::schema::columns::validate_postgres_relation_column_type(column, ty)?;
            }
        }
        let name = context.namespace.resolve_persistent_name(name)?;
        if let Some(kind) = context
            .names
            .relation_kind_at(&name)
            .map_err(|error| SQLError::Internal(format!("resolve relation `{name}`: {error}")))?
        {
            if if_not_exists {
                return Ok(None);
            }
            return Err(SQLError::Routine {
                sqlstate: "42P07".into(),
                message: format!("relation \"{name}\" already exists as {kind}"),
            });
        }
        context.namespace.ensure_create(&name)?;
        let materialized_column_types = query_schema.column_types().to_vec();
        let materialized_rows = if with_no_data {
            Vec::new()
        } else {
            let executable = context.queries.optimize(&plan)?;
            let result = context.queries.execute(&executable, params)?;
            materialized_rows(&result, &output_columns)?
        };
        let affected_rows = u64::try_from(materialized_rows.len())
            .map_err(|_| SQLError::Internal("materialized-view row count exceeds u64".into()))?;
        let relation = RelationIdentity::from_legacy_name(&name).map_err(|error| {
            SQLError::Internal(format!("invalid materialized-view name: {error}"))
        })?;
        let view = StoredView {
            object_id: context.catalog.allocate_identity().map_err(|error| {
                SQLError::Internal(format!(
                    "allocate materialized view `{name}` identity: {error}"
                ))
            })?,
            role_owner: context.access.current_user_name(),
            acl: None,
            column_acls: std::collections::BTreeMap::new(),
            query: plan,
            output_columns: Some(output_columns),
            persistence: uqa_sql::ast::RelationPersistence::Permanent,
            options: options.to_vec(),
            kind: StoredViewKind::Materialized,
            materialized_rows,
            materialized_column_types,
            populated: !with_no_data,
        };
        publication::publish_materialized_view(
            context.publication,
            context.changes,
            relation,
            view,
            &name,
        )?;
        Ok((!with_no_data).then_some(affected_rows))
    }))
}

pub fn refresh_materialized_view(
    transactions: &dyn ViewCreationTransactions,
    name: &str,
    concurrently: bool,
    with_no_data: bool,
) -> Result<(), SQLError> {
    if concurrently {
        return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "REFRESH MATERIALIZED VIEW CONCURRENTLY requires a qualifying unique index, which is not available".into(),
            });
    }
    transactions.with_view_creation(Box::new(move |context| {
        let (canonical, kind) = context
            .names
            .resolve_relation_kind(name)?
            .into_found()
            .ok_or_else(|| SQLError::Routine {
                sqlstate: "42P01".into(),
                message: format!("relation \"{name}\" does not exist"),
            })?;
        if kind != "materialized view" {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: format!("\"{name}\" is not a materialized view"),
            });
        }
        let relation = RelationIdentity::from_legacy_name(&canonical).map_err(|error| {
            SQLError::Internal(format!("invalid materialized-view name: {error}"))
        })?;
        let mut view = context.views.view(&relation).ok_or_else(|| {
            SQLError::Internal(format!("materialized view `{canonical}` disappeared"))
        })?;
        context.access.ensure_maintenance(&canonical, &view)?;
        view.materialized_rows = if with_no_data {
            Vec::new()
        } else {
            let result = context.query_owners.with_owner(
                &view.role_owner,
                Box::new(|queries| queries.execute(&view.query, &[])),
            )?;
            let output_columns = view.output_columns.as_deref().ok_or_else(|| {
                SQLError::Internal(format!(
                    "loaded materialized view `{canonical}` has no durable public column metadata"
                ))
            })?;
            let rows = materialized_rows(&result, output_columns)?;
            view.materialized_column_types = result.column_types;
            rows
        };
        view.populated = !with_no_data;
        publication::publish_materialized_view(
            context.publication,
            context.changes,
            relation,
            view,
            &canonical,
        )?;
        Ok(())
    }))
}

#[cfg(test)]
mod tests;
