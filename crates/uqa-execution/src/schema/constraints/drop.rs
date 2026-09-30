//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Remove constraints and their referencing keys in dependency order.
use super::{
    constraint_error, ddl_storage_error, find_constraint, publish_constraint_state,
    table_constraint_state, ConstraintAlterContext, ConstraintLocation, SQLError,
};
use uqa_sql::schema::constraint_changes::{
    foreign_key_target::ForeignKeyTarget, inheritance::ensure_inherited_constraint_removable,
};
mod foreign_keys;

pub fn drop_constraint(
    context: &ConstraintAlterContext<'_>,
    table: &str,
    name: &str,
    if_exists: bool,
    cascade: bool,
    recurse: bool,
) -> Result<(), SQLError> {
    let table = context
        .publication
        .catalog
        .resolve_table_name(table)
        .map_err(|error| ddl_storage_error("DROP CONSTRAINT relation lookup", error))?
        .unwrap_or_else(|| table.to_string());
    ensure_partitions_not_in_use(context, &table)?;
    if let Some(trigger) = context.access.constraint_trigger_name(&table, name)? {
        let relation = uqa_core::RelationIdentity::from_legacy_name(&table).map_err(|error| {
            SQLError::Internal(format!(
                "decode constraint-trigger relation `{table}`: {error}"
            ))
        })?;
        return Err(SQLError::Diagnostic {
            sqlstate: "2BP01".into(),
            message: format!(
                "cannot drop constraint {name} on table {} because trigger {trigger} on table {} requires it",
                relation.name, relation.name
            ),
            detail: None,
            hint: Some(format!(
                "You can drop trigger {trigger} on table {} instead.",
                relation.name
            )),
        });
    }
    {
        let (columns, constraints) = table_constraint_state(context, &table)?;
        if find_constraint(&columns, &constraints, name).is_none() {
            let relation =
                uqa_core::RelationIdentity::from_legacy_name(&table).map_err(SQLError::Internal)?;
            if if_exists {
                context
                    .notices
                    .lock()
                    .push(uqa_sql::SQLNotice::notice(format!(
                        "constraint \"{name}\" of relation \"{}\" does not exist, skipping",
                        relation.name
                    )));
                return Ok(());
            }
            return Err(constraint_error(
                "42704",
                format!(
                    "constraint \"{name}\" of relation \"{}\" does not exist",
                    relation.name
                ),
            ));
        }
    }
    if super::inheritance::drop_inherited_constraint(context, &table, name, recurse, cascade)? {
        return Ok(());
    }
    ensure_direct_constraint_removal(context, &table, name)?;
    perform_constraint_deletion(context, &table, name, cascade)
}

/// `ATCheckPartitionsNotInUse`: no partition of a partitioned table may have pending trigger events.
fn ensure_partitions_not_in_use(
    context: &ConstraintAlterContext<'_>,
    table: &str,
) -> Result<(), SQLError> {
    if context
        .relations
        .table_hierarchy(table)
        .map_err(|error| ddl_storage_error("DROP CONSTRAINT hierarchy", error))?
        .partition_spec
        .is_none()
    {
        return Ok(());
    }
    for partition in context
        .rows
        .catalog
        .hierarchy_scan_tables(table, true)?
        .into_iter()
        .filter(|partition| partition != table)
    {
        context
            .access
            .ensure_no_pending_events(&partition, "ALTER TABLE")?;
    }
    Ok(())
}

/// `dropconstraint_internal`'s `performDeletion` of one table constraint and what depends on it.
pub(super) fn perform_constraint_deletion(
    context: &ConstraintAlterContext<'_>,
    table: &str,
    name: &str,
    cascade: bool,
) -> Result<(), SQLError> {
    let relation =
        uqa_core::RelationIdentity::from_legacy_name(table).map_err(SQLError::Internal)?;
    crate::schema::deletion::perform_deletion(
        &context.deletion.catalog_removal_context(),
        |dependencies| {
            Ok(vec![crate::schema::deletion::required_address(
                dependencies.relation_member_address(
                    uqa_sql::catalog::dependencies::CONSTRAINT_CLASS,
                    &relation,
                    name,
                ),
                || format!("constraint {name} on {table}"),
            )?])
        },
        cascade,
    )
}

/// `dropconstraint_internal`'s check of a constraint `ALTER TABLE DROP CONSTRAINT` names directly: a partition's copy of its parent's foreign key or key is inherited and goes with the parent's.
fn ensure_direct_constraint_removal(
    context: &ConstraintAlterContext<'_>,
    table: &str,
    name: &str,
) -> Result<(), SQLError> {
    let (columns, constraints) = table_constraint_state(context, table)?;
    if let Some(target) = ForeignKeyTarget::by_name(&columns, &constraints, name)? {
        return foreign_keys::ensure_direct_removal(context, table, name, target.object_id);
    }
    if let Some(key) = constraints
        .key_constraints
        .iter()
        .find(|key| key.name.as_deref() == Some(name))
    {
        let catalog = context
            .publication
            .indexes
            .identities
            .catalog
            .current_catalog_snapshot();
        for row in catalog.catalog_indexes() {
            let definition = crate::catalog::index::index_definition(row)
                .map_err(|error| ddl_storage_error("DROP CONSTRAINT index", error))?;
            if key.catalog_identity.is_some_and(|identity| {
                definition.relationships.owning_constraint == Some(identity.object_id)
            }) && definition.relationships.parent_index.is_some()
            {
                return ensure_inherited_constraint_removable(table, name, 1);
            }
        }
    }
    Ok(())
}

/// Remove one constraint whose dependents were removed before it: a foreign key with its partitions' copies, or another constraint with its implementing index.
pub fn drop_constraint_dependency(
    context: &ConstraintAlterContext<'_>,
    table: &str,
    name: &str,
) -> Result<(), SQLError> {
    let (columns, constraints) = table_constraint_state(context, table)?;
    if ForeignKeyTarget::by_name(&columns, &constraints, name)?.is_some() {
        let targets = foreign_keys::capture_foreign_key_dependencies(
            context,
            [(table.to_string(), name.to_string())],
        )?;
        return foreign_keys::drop_targets(context, targets);
    }
    drop_constraint_one(context, table, name)
}

/// Remove a constraint that is not a foreign key from its table's metadata; the objects that depend on it, including the foreign keys that reference its index, were removed before it.
fn drop_constraint_one(
    context: &ConstraintAlterContext<'_>,
    table: &str,
    name: &str,
) -> Result<(), SQLError> {
    let (mut columns, mut constraints) = table_constraint_state(context, table)?;
    let Some(location) = find_constraint(&columns, &constraints, name) else {
        return Ok(());
    };
    match location {
        ConstraintLocation::NotNull(index) => {
            uqa_sql::schema::constraint_changes::not_null_removal::validate_constraint_removal(
                table,
                &columns[index],
                &constraints,
            )?;
            columns[index].not_null = false;
            columns[index].not_null_explicit = false;
            columns[index].not_null_name = None;
            columns[index].not_null_identity = None;
            columns[index].not_null_validated = true;
            columns[index].not_null_no_inherit = false;
            columns[index].not_null_is_local = true;
        }
        ConstraintLocation::ColumnCheck(index) => {
            columns[index].check = None;
            columns[index].check_name = None;
            columns[index].check_enforced = true;
            columns[index].check_validated = true;
            columns[index].check_no_inherit = false;
            columns[index].check_is_local = true;
            columns[index].check_object_id = None;
            columns[index].check_catalog_oid = None;
        }
        ConstraintLocation::ColumnForeignKey(index) => columns[index].references = None,
        ConstraintLocation::TableCheck(index) => {
            constraints.checks.remove(index);
        }
        ConstraintLocation::TableForeignKey(index) => {
            constraints.foreign_keys.remove(index);
        }
        ConstraintLocation::Key(index) => {
            let key = constraints.key_constraints[index].clone();
            constraints.key_constraints.remove(index);
            constraints
                .hierarchy
                .partition_inherited_key_constraints
                .retain(|inherited| {
                    !uqa_sql::schema::constraint_metadata::identity::keys::provenance_matches(
                        &key, inherited,
                    )
                });
            if key.columns.len() == 1 {
                if let Some(column) = columns
                    .iter_mut()
                    .find(|column| column.name == key.columns[0])
                {
                    match key.kind {
                        uqa_sql::ast::TableKeyConstraintKind::PrimaryKey => {
                            column.primary_key = false;
                        }
                        uqa_sql::ast::TableKeyConstraintKind::Unique => {
                            column.unique = false;
                        }
                    }
                }
            }
        }
    }
    publish_constraint_state(context, table, columns, constraints)
}
