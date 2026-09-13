//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execute bound index removal, dependent constraints and physical field publication.
use uqa_sql::{ast::DropStmt, SQLError, SQLResult};
use uqa_storage::CatalogIndexRow;
mod context;
pub use context::*;

pub fn run_drop_index(
    context: &IndexRemovalContext<'_>,
    stmt: DropStmt,
) -> Result<SQLResult, SQLError> {
    let mut indexes = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for requested in &stmt.names {
        if let Some(canonical) = uqa_sql::schema::indexes::removal::resolve_drop_index_name(
            context.catalog.resolve_relation_kind(requested)?,
            requested,
            stmt.if_exists,
            &mut |message| {
                context
                    .notices
                    .lock()
                    .push(("NOTICE".to_string(), message.to_string()));
            },
        )? {
            let relation = uqa_core::RelationIdentity::from_legacy_name(&canonical)
                .map_err(SQLError::Internal)?;
            if !seen.insert(relation.clone()) {
                continue;
            }
            let row = context
                .catalog
                .bound_catalog_index(&canonical)
                .map_err(|error| ddl_storage_error("DROP INDEX", error))?
                .ok_or_else(|| {
                    SQLError::Internal(format!(
                        "resolved index `{canonical}` has no bound catalog row"
                    ))
                })?;
            context.privileges.ensure_drop_authority(&row)?;
            uqa_sql::schema::indexes::removal::ensure_index_not_constraint_owned(
                &row.relation,
                &row.table_name,
                context.catalog.has_constraint_index(&row.relation),
            )?;
            indexes.push(row);
        }
    }
    let mut dependents = std::collections::BTreeSet::new();
    for index in &indexes {
        let referrers = context
            .referrers
            .referrers_to(&index.table_name)
            .map_err(|error| {
                SQLError::Internal(format!("index foreign-key dependencies: {error}"))
            })?;
        uqa_sql::schema::indexes::removal::collect_index_dependents(
            &index.relation.name,
            referrers,
            stmt.cascade,
            &mut dependents,
        )?;
    }
    for row in &indexes {
        context.locks.lock_exclusive(&row.table_name)?;
    }
    context
        .transactions
        .with_index_write(Box::new(move |context| {
            for (table, name) in dependents {
                crate::schema::constraints::drop::drop_constraint_dependency(
                    &context.constraints,
                    &table,
                    &name,
                )?;
            }
            for row in indexes {
                drop_index_side_effects(context, &row)?;
                context
                    .publication
                    .drop_catalog_index_relation(&row.relation)
                    .map_err(|error| ddl_storage_error("DROP INDEX", error))?;
            }
            Ok(SQLResult::empty())
        }))
}

fn ddl_storage_error(action: &str, err: impl std::error::Error + 'static) -> SQLError {
    uqa_sql::catalog::errors::storage_error(action, &err)
}

fn drop_index_side_effects(
    context: &IndexRemovalContext<'_>,
    row: &CatalogIndexRow,
) -> Result<(), SQLError> {
    if row.index_type.eq_ignore_ascii_case("gin") {
        drop_gin_index_side_effects(context, row)?;
    } else if row.index_type.eq_ignore_ascii_case("ivf")
        || row.index_type.eq_ignore_ascii_case("hnsw")
    {
        drop_vector_index_side_effects(context, row)?;
    }
    Ok(())
}

fn drop_gin_index_side_effects(
    context: &IndexRemovalContext<'_>,
    row: &CatalogIndexRow,
) -> Result<(), SQLError> {
    let fields: std::collections::BTreeSet<String> =
        uqa_sql::schema::indexes::removal::catalog_index_columns(
            &row.relation,
            &row.columns_json,
            "DROP INDEX",
        )?
        .into_iter()
        .collect();
    let indexes = context
        .catalog
        .list_catalog_indexes()
        .map_err(|err| ddl_storage_error("DROP INDEX", err))?;

    for field in fields {
        let still_referenced = uqa_sql::schema::indexes::removal::gin_field_is_referenced(
            &row.relation,
            &row.table_name,
            &field,
            indexes.iter().map(|candidate| {
                uqa_sql::schema::indexes::removal::IndexRemovalCandidate {
                    relation: &candidate.relation,
                    table: &candidate.table_name,
                    method: &candidate.index_type,
                    columns_json: &candidate.columns_json,
                }
            }),
        )?;
        if still_referenced {
            let mut named_owner_remains = false;
            for candidate in &indexes {
                if candidate.relation == row.relation
                    || candidate.table_name != row.table_name
                    || !candidate.index_type.eq_ignore_ascii_case("gin")
                {
                    continue;
                }
                let columns = uqa_sql::schema::indexes::removal::catalog_index_columns(
                    &candidate.relation,
                    &candidate.columns_json,
                    "DROP INDEX",
                )?;
                if !columns.contains(&field) {
                    continue;
                }
                let parameters: std::collections::BTreeMap<String, String> =
                    serde_json::from_str(&candidate.parameters_json).map_err(|error| {
                        SQLError::Internal(format!("invalid GIN parameters: {error}"))
                    })?;
                named_owner_remains |= parameters
                    .keys()
                    .any(|name| name.eq_ignore_ascii_case("analyzer"));
            }
            if !named_owner_remains {
                context
                    .publication
                    .release_fts_analyzer_owner(&row.table_name, &field)
                    .map_err(SQLError::Internal)?;
            }
        }
        if !still_referenced {
            context
                .publication
                .drop_fts_field(&row.table_name, &field)
                .map_err(|err| {
                    SQLError::Internal(format!(
                        "DROP INDEX `{}`: failed to remove FTS field `{}`.`{field}`: {err}",
                        row.relation.qualified_name(),
                        row.table_name
                    ))
                })?;
        }
    }
    Ok(())
}

fn drop_vector_index_side_effects(
    context: &IndexRemovalContext<'_>,
    row: &CatalogIndexRow,
) -> Result<(), SQLError> {
    let columns = uqa_sql::schema::indexes::removal::catalog_index_columns(
        &row.relation,
        &row.columns_json,
        "DROP INDEX",
    )?;
    for col in columns {
        let column_type = context
            .catalog
            .column_type(&row.table_name, &col)
            .map_err(|err| ddl_storage_error("DROP INDEX", err))?;
        let dim = uqa_sql::schema::indexes::removal::vector_index_dimensions(
            &row.relation,
            &row.table_name,
            &col,
            column_type,
        )?;
        if !context
            .publication
            .drop_vector_field_index(&row.table_name, col.clone(), dim)
            .map_err(|err| ddl_storage_error("DROP INDEX vector field", err))?
        {
            return Err(SQLError::Unsupported(format!(
                "DROP INDEX `{}`: relation `{}` does not exist",
                row.relation.qualified_name(),
                row.table_name
            )));
        }
        context
            .publication
            .drop_vector_index_metadata(&row.table_name, &col)
            .map_err(|e| {
                SQLError::Internal(format!(
                    "DROP INDEX `{}`: failed to drop vector-index metadata for `{}`.`{col}`: {e}",
                    row.relation.qualified_name(),
                    row.table_name
                ))
            })?;
    }
    Ok(())
}

/// Remove a dependent index after the owning DROP command has checked its authority.
pub fn drop_index_dependency(
    context: &IndexRemovalContext<'_>,
    relation: &uqa_core::RelationIdentity,
) -> Result<(), SQLError> {
    let row = context
        .catalog
        .bound_catalog_index(&relation.qualified_name())
        .map_err(|error| ddl_storage_error("DROP INDEX dependency", error))?
        .ok_or_else(|| SQLError::Internal("dependent index disappeared".into()))?;
    drop_index_side_effects(context, &row)?;
    context
        .publication
        .drop_catalog_index_relation(relation)
        .map_err(|error| ddl_storage_error("DROP INDEX dependency", error))?;
    Ok(())
}
