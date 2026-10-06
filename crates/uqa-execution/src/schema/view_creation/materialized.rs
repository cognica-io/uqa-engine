//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Materialized-view source execution and snapshot replacement lifecycle.
use super::{
    context::{ViewCreationContext, ViewCreationTransactions},
    publication,
    registration::reject_regrole_constants,
    MaterializedViewRegistration,
};
use crate::catalog::view::{StoredView, StoredViewKind};
use crate::row_locks::{
    binding::{bind_relation, RelationBinding},
    RelationLockMode,
};
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::RelationPersistence,
    schema::table_creation::{
        create_table_as_columns, existing_create_as_target, validate_create_table_as_columns,
    },
    SQLError,
};

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

fn skip_existing_materialized_view(
    context: &ViewCreationContext<'_>,
    name: &str,
    if_not_exists: bool,
) -> Result<bool, SQLError> {
    let resolved = if context
        .namespace
        .targets_temporary_namespace(name, RelationPersistence::Permanent)?
    {
        context.namespace.temporary_name(name)?
    } else {
        context.namespace.resolve_persistent_name(name)?
    };
    if context
        .names
        .relation_kind_at(&resolved)
        .map_err(|error| SQLError::Internal(format!("resolve relation `{resolved}`: {error}")))?
        .is_some()
    {
        context
            .notices
            .push(existing_create_as_target(name, if_not_exists)?);
        return Ok(true);
    }
    Ok(false)
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
        let owner = context.namespace.bind_owner()?;
        context.bindings.lock_relations(&plan)?;
        if context.bindings.bind_relations(&mut plan)? {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "materialized views must not use temporary tables or views".into(),
            });
        }
        let query_schema = context.bindings.bind_routines(&mut plan, params)?;
        context.bindings.bind_type_identities(&mut plan)?;
        reject_regrole_constants(context, &mut plan)?;
        // PostgreSQL analyzes the source before CreateTableAsRelExists, then builds the column list before DefineRelation checks schema CREATE and column validity.
        if skip_existing_materialized_view(context, name, if_not_exists)? {
            return Ok(None);
        }
        let columns = create_table_as_columns(&query_schema, column_names)?;
        // Re-resolve the original name and authority after namespace waits; skipped targets never retain a namespace dependency.
        let (name, persistence) = context
            .namespace
            .relation_target(name, RelationPersistence::Permanent)?;
        validate_create_table_as_columns(context.routines, &columns)?;
        let output_columns = columns
            .into_iter()
            .map(|column| column.name)
            .collect::<Vec<_>>();
        context.namespace.retain_owner(&owner)?;
        context.namespace.ensure_create(&name)?;
        context.namespace.reserve_row_type_name(&name)?;
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
        context.locks.prepare_definition_write()?;
        context.namespace.ensure_create(&name)?;
        let catalog_oids = super::registration::allocate_view_oids(context, &relation)?;
        let view = StoredView {
            security: uqa_sql::catalog::security::BoundTableSecurity::owner(owner.identity()),
            definition: uqa_sql::catalog::stored_view::StoredViewDefinition {
                object_id: context.catalog.allocate_identity().map_err(|error| {
                    SQLError::Internal(format!(
                        "allocate materialized view `{name}` identity: {error}"
                    ))
                })?,
                query: plan,
                output_columns: Some(output_columns),
                persistence,
                options: options.to_vec(),
                kind: StoredViewKind::Materialized,
                materialized_rows,
                materialized_column_types,
                populated: !with_no_data,
                catalog_oids: Some(catalog_oids),
                row_type_array_name: Some(crate::schema::types::arrays::reserve_array_name(
                    &context.namespace,
                    &relation.schema,
                    &relation.name,
                )?),
            },
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
        let binding = bind_relation(
            context.locks,
            RelationLockMode::AccessExclusive,
            false,
            || {
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
                let relation =
                    RelationIdentity::from_legacy_name(&canonical).map_err(SQLError::Internal)?;
                let view = context.views.view(&relation).ok_or_else(|| {
                    SQLError::Internal(format!("materialized view `{canonical}` disappeared"))
                })?;
                Ok(Some(RelationBinding {
                    name: canonical,
                    object_id: Some(view.object_id),
                    value: (relation, view),
                }))
            },
            |binding| {
                context
                    .access
                    .ensure_maintenance(&binding.name, &binding.value.1)
            },
        )?
        .ok_or_else(|| SQLError::Internal("materialized view binding disappeared".into()))?;
        let canonical = binding.name;
        let (relation, mut view) = binding.value;
        view.materialized_rows = if with_no_data {
            Vec::new()
        } else {
            context.bindings.lock_relations(&view.query)?;
            let owner = view
                .security
                .owner_reference(&context.namespace.roles.role_definitions())?;
            let result = context.query_owners.with_owner(
                &owner,
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
        context.locks.prepare_definition_write()?;
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
