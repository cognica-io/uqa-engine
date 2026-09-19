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
    Ok(())
}
