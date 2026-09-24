//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execute bound index removal, dependent constraints and physical field publication.
use uqa_sql::{ast::DropStmt, SQLError, SQLResult};
use uqa_storage::CatalogIndexRow;
mod binding;
mod context;
mod tree;
pub use context::*;

pub fn run_drop_index(
    context: &IndexRemovalContext<'_>,
    stmt: DropStmt,
) -> Result<SQLResult, SQLError> {
    let indexes = binding::bind_drop_targets(
        context.catalog,
        context.privileges,
        context.constraints.lock_session,
        &stmt,
        &mut |message| {
            context
                .notices
                .lock()
                .push(("NOTICE".to_string(), message.to_string()));
        },
    )?;
    for index in &indexes {
        lock_index_partitions(context, &index.table_name)?;
    }
    for index in &indexes {
        uqa_sql::schema::indexes::removal::ensure_index_not_constraint_owned(
            &index.relation,
            &index.table_name,
            context.catalog.has_constraint_index(&index.relation),
        )?;
    }
    let tree = tree::bind_removals(context, &indexes, stmt.cascade)?;
    let mut dependents = std::collections::BTreeSet::new();
    for index in tree.values() {
        let referrers = context
            .referrers
            .referrers_to(&index.table_name)
            .map_err(|error| {
                SQLError::Internal(format!("index foreign-key dependencies: {error}"))
            })?;
        uqa_sql::schema::indexes::removal::collect_index_dependents(
            &index.relation.name,
            crate::catalog::index::index_definition(index)
                .map_err(|error| ddl_storage_error("DROP INDEX identity", error))?
                .catalog
                .ok_or_else(|| SQLError::Internal("index has no identity".into()))?
                .identity
                .object_id,
            referrers,
            stmt.cascade,
            &mut dependents,
        )?;
    }
    let targets = crate::schema::constraints::drop::capture_foreign_key_dependencies(
        &context.constraints,
        dependents,
    )?;
    context
        .transactions
        .with_index_write(Box::new(move |context| {
            crate::schema::constraints::drop::drop_foreign_key_dependencies(
                &context.constraints,
                targets,
            )?;
            drop_tree_side_effects(context, &tree)?;
            for row in indexes {
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

fn lock_index_partitions(context: &IndexRemovalContext<'_>, table: &str) -> Result<(), SQLError> {
    if context
        .constraints
        .relations
        .table_hierarchy(table)
        .map_err(|error| ddl_storage_error("DROP INDEX hierarchy", error))?
        .partition_spec
        .is_none()
    {
        return Ok(());
    }
    let descendants = context
        .constraints
        .rows
        .catalog
        .hierarchy_scan_tables(table, true)?;
    crate::row_locks::binding::lock_descendants(
        context.constraints.lock_catalog,
        context.constraints.lock_session,
        descendants.into_iter().filter(|child| child != table),
        crate::row_locks::RelationLockMode::AccessExclusive,
        false,
    )
}

fn drop_index_side_effects(
    context: &IndexRemovalContext<'_>,
    row: &CatalogIndexRow,
    survivors: &[CatalogIndexRow],
    removed_fields: &mut std::collections::BTreeSet<(String, String)>,
) -> Result<(), SQLError> {
    if row.index_type.eq_ignore_ascii_case("gin") {
        drop_gin_index_side_effects(context, row, survivors, removed_fields)?;
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
    indexes: &[CatalogIndexRow],
    removed_fields: &mut std::collections::BTreeSet<(String, String)>,
) -> Result<(), SQLError> {
    let fields: std::collections::BTreeSet<String> =
        uqa_sql::schema::indexes::removal::catalog_index_columns(
            &row.relation,
            &row.columns_json,
            "DROP INDEX",
        )?
        .into_iter()
        .collect();
    for field in fields {
        if !removed_fields.insert((row.table_name.clone(), field.clone())) {
            continue;
        }
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
            for candidate in indexes {
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
                    .release_fts_analyzer_owner(&row.table_name, &field)?;
            }
        }
        if !still_referenced {
            context
                .publication
                .drop_fts_field(&row.table_name, &field)
                .map_err(|err| {
                    if matches!(err, SQLError::Cancelled(_)) {
                        return err;
                    }
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
    let tree = tree::bind_removals(context, &[row], true)?;
    drop_tree_side_effects(context, &tree)?;
    context
        .publication
        .drop_catalog_index_relation(relation)
        .map_err(|error| ddl_storage_error("DROP INDEX dependency", error))?;
    Ok(())
}

fn drop_tree_side_effects(
    context: &IndexRemovalContext<'_>,
    tree: &std::collections::BTreeMap<uqa_core::RelationIdentity, CatalogIndexRow>,
) -> Result<(), SQLError> {
    let survivors = context
        .catalog
        .list_catalog_indexes()
        .map_err(|error| ddl_storage_error("DROP INDEX survivors", error))?
        .into_iter()
        .filter(|row| !tree.contains_key(&row.relation))
        .collect::<Vec<_>>();
    let mut removed_fields = std::collections::BTreeSet::new();
    for row in tree.values() {
        drop_index_side_effects(context, row, &survivors, &mut removed_fields)?;
    }
    Ok(())
}
