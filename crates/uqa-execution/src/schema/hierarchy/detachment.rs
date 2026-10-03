//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Allocate independent enforcement families before publishing detached constraint rows.

use std::collections::{BTreeMap, BTreeSet};
use uqa_sql::{
    catalog::constraints::{foreign_key_identity, ConstraintIdentity},
    schema::inheritance::detachment::ConstraintIdentityChange,
    SQLError,
};

pub trait DetachedConstraintModes {
    fn preserve_split_modes(
        &self,
        retained: &[ConstraintIdentity],
        detached: &[ConstraintIdentityChange],
    ) -> Result<(), SQLError>;
}

/// Refuse to detach `partition` while a row of a referencing table references a key in its subtree through a constraint derived on it, as `PostgreSQL`'s `ATDetachCheckNoForeignKeyRefs` does, whether the foreign key is enforced or not. Each such referencing table is locked `Share` until the transaction ends, and the rows are read in their latest committed state with the transaction's own changes.
pub(super) fn ensure_no_referencing_rows(
    context: &super::HierarchyContext<'_>,
    partition: &str,
) -> Result<(), SQLError> {
    use crate::schema::publication::referenced_partitions::{
        declared_foreign_keys, declared_state,
    };
    let partition_id = context
        .partitions
        .catalog
        .try_table_object_id(partition)
        .map_err(SQLError::Internal)?
        .ok_or_else(|| SQLError::UnknownTable(partition.to_string()))?;
    let tables = context
        .publication
        .referencing
        .table_names()
        .map_err(|error| super::ddl_storage_error("DETACH PARTITION foreign keys", error))?;
    for table in tables {
        let (columns, constraints) = declared_state(&context.publication, &table)?;
        for foreign_key in declared_foreign_keys(&context.publication, &columns, &constraints)? {
            let Some(derived) = foreign_key
                .referenced_partitions
                .iter()
                .find(|derived| derived.partition == partition_id)
            else {
                continue;
            };
            context
                .namespace
                .lock_relation(&table, uqa_sql::ast::TableLockMode::Share)?;
            if let Some((physical_table, values)) =
                referencing_row(context, &table, &foreign_key, partition)?
            {
                let detail = crate::mutation::constraints::foreign_key_key(
                    context.constraints,
                    partition,
                    &foreign_key.ref_columns,
                    &physical_table,
                    &foreign_key.local_columns,
                    &values,
                )?
                .map(|key| {
                    format!(
                        "{key} is still referenced from table \"{}\".",
                        super::local_relation_name(&table)
                    )
                });
                return Err(SQLError::Diagnostic {
                    sqlstate: "23503".into(),
                    message: format!(
                        "removing partition \"{}\" violates foreign key constraint \"{}\"",
                        super::local_relation_name(partition),
                        derived.name
                    ),
                    detail,
                    hint: None,
                });
            }
        }
    }
    Ok(())
}

/// The first row of `table`, a referencing table, and its key values, whose key the subtree of `partition` holds.
fn referencing_row(
    context: &super::HierarchyContext<'_>,
    table: &str,
    foreign_key: &uqa_sql::ast::ForeignKey,
    partition: &str,
) -> Result<Option<(String, Vec<uqa_core::Value>)>, SQLError> {
    let mut scoped = foreign_key.clone();
    scoped.ref_table = partition.to_string();
    for physical_table in
        uqa_sql::semantics::partition::foreign_key_scan_tables(context.partitions.catalog, table)?
    {
        for doc_id in context
            .constraints
            .reads
            .live_table_doc_ids(&physical_table)?
        {
            let Some(document) = context
                .constraints
                .reads
                .get_document(&physical_table, doc_id)?
            else {
                continue;
            };
            let Some(lookup) = uqa_sql::semantics::foreign_keys::foreign_key_lookup_values(
                context.partitions.catalog,
                &physical_table,
                &scoped,
                &document,
            )?
            else {
                continue;
            };
            let referenced = if scoped.period {
                crate::mutation::constraints::period::period_foreign_key_overlap(
                    context.constraints,
                    &scoped,
                    &lookup.values,
                )?
            } else {
                crate::mutation::constraints::find_foreign_key_parent(
                    context.constraints,
                    &scoped,
                    &lookup,
                )?
                .is_some()
            };
            if referenced {
                let values = foreign_key
                    .local_columns
                    .iter()
                    .map(|column| {
                        document
                            .get(column)
                            .cloned()
                            .unwrap_or(uqa_core::Value::Null)
                    })
                    .collect();
                return Ok(Some((physical_table, values)));
            }
        }
    }
    Ok(None)
}

pub(super) struct DetachedFamilies {
    pub retained: Vec<ConstraintIdentity>,
    pub families: BTreeMap<[u8; 16], [u8; 16]>,
}

pub(super) fn prepare(
    context: &super::HierarchyContext<'_>,
    parent: &str,
    partition: &str,
    subtree: &[String],
) -> Result<DetachedFamilies, SQLError> {
    for target in subtree {
        context
            .constraint_access
            .ensure_no_pending_events(target, "ALTER TABLE")?;
    }
    let child_ids: BTreeSet<_> = context
        .catalog
        .try_foreign_keys(partition)
        .map_err(|error| super::ddl_storage_error("DETACH PARTITION foreign keys", error))?
        .into_iter()
        .filter_map(|key| key.object_id)
        .collect();
    let parents = context
        .catalog
        .try_foreign_keys(parent)
        .map_err(|error| super::ddl_storage_error("DETACH PARTITION foreign keys", error))?;
    let mut retained = Vec::new();
    let mut families = BTreeMap::new();
    for key in parents {
        let identity = foreign_key_identity(parent, &key)?;
        let object_id = identity.object_id.ok_or_else(|| {
            SQLError::Internal("materialized parent foreign key has no incarnation".into())
        })?;
        if child_ids.contains(&object_id) {
            let replacement =
                crate::catalog::identity::allocate_catalog_object_id("detached foreign-key family")
                    .map_err(|error| {
                        uqa_sql::catalog::errors::storage_error("DETACH PARTITION identity", &error)
                    })?;
            families.insert(object_id, replacement);
            retained.push(identity);
        }
    }
    Ok(DetachedFamilies { retained, families })
}

pub(super) fn publish(
    context: &super::HierarchyContext<'_>,
    parent: &str,
    partition: &str,
    parent_spec: &uqa_sql::ast::PartitionSpec,
    bound: &uqa_sql::ast::PartitionBound,
    concurrently: bool,
) -> Result<(), SQLError> {
    use super::{ddl_storage_error, declared_constraints, table_columns, HierarchySchemaChange};
    use uqa_sql::ast::AutoIncrement;
    use uqa_sql::schema::inheritance::alter::{
        clear_partition_constraint_provenance, detached_bound_check, restore_identity_overrides,
    };
    let inherited_identity = table_columns(context, parent, "DETACH PARTITION")?
        .into_iter()
        .filter_map(|column| {
            column
                .auto_increment
                .filter(AutoIncrement::is_identity)
                .map(|increment| (column.name, increment))
        })
        .collect::<Vec<_>>();
    let subtree = context
        .constraints
        .catalog
        .hierarchy_scan_tables(partition, true)?;
    let split = prepare(context, parent, partition, &subtree)?;
    let mut mode_changes = Vec::new();
    for target in &subtree {
        let mut columns = table_columns(context, target, "DETACH PARTITION")?;
        let mut constraints = declared_constraints(context, target, "DETACH PARTITION")?;
        restore_identity_overrides(
            &mut columns,
            &inherited_identity,
            &constraints.hierarchy.partition_identity_overrides,
        );
        mode_changes.extend(
            uqa_sql::schema::inheritance::detachment::split_foreign_key_families(
                target,
                &mut columns,
                &mut constraints,
                &split.families,
            )?,
        );
        if target == partition {
            clear_partition_constraint_provenance(&mut constraints);
        }
        if concurrently {
            constraints.checks.push(detached_bound_check(
                target,
                parent_spec,
                bound,
                &constraints.checks,
            ));
        }
        let mut hierarchy = constraints.hierarchy.clone();
        hierarchy.partition_identity_overrides.clear();
        if target == partition {
            hierarchy.parents.clear();
            hierarchy.parent_sequence_numbers.clear();
            hierarchy.partition_bound = None;
        }
        crate::schema::publication::hierarchy::replace_hierarchy_components(
            &context.publication,
            context.catalog,
            target,
            HierarchySchemaChange {
                columns,
                checks: constraints.checks,
                foreign_keys: constraints.foreign_keys,
                key_constraints: constraints.key_constraints,
                hierarchy,
            },
        )
        .map_err(|error| ddl_storage_error("DETACH PARTITION", error))?;
    }
    context
        .constraint_modes
        .preserve_split_modes(&split.retained, &mode_changes)?;
    // The constraints the foreign keys referencing the parent or its ancestors derived on the detached subtree go with it.
    crate::schema::publication::referenced_partitions::republish_referencing_tables(
        &context.publication,
        parent,
        crate::row_locks::RelationLockMode::AccessExclusive,
    )
}
