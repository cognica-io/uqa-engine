//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Repair legacy hierarchy candidates before publishing any restored constraint rows.

use super::{metadata_foreign_keys, ConstraintMetadataMigration};
use std::collections::{BTreeMap, BTreeSet};
use uqa_sql::schema::inheritance::{detachment::split_foreign_key_families, restoration};
use uqa_storage::{StorageBackendError, StorageBackendResult};

pub(super) fn repair(migrations: &mut [ConstraintMetadataMigration]) -> StorageBackendResult<()> {
    let existing = migrations
        .iter()
        .map(|migration| migration.schema.relation.qualified_name())
        .collect();
    let mut repairs = Vec::with_capacity(migrations.len());
    for migration in &mut *migrations {
        let repair = restoration::repair_parent_edges(
            &mut migration.columns,
            &mut migration.constraints,
            &existing,
        )
        .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        migration.changed |= repair.changed;
        repairs.push(repair);
    }
    super::synchronize_inherited_constraint_object_ids(migrations);
    repair_origins(migrations, &repairs);
    for (root, repair) in repairs.into_iter().enumerate() {
        if repair.detached_partition {
            split_detached_tree(migrations, root, &repair)?;
        }
    }
    Ok(())
}

fn repair_origins(
    migrations: &mut [ConstraintMetadataMigration],
    repairs: &[restoration::ParentEdgeRepair],
) {
    if !repairs.iter().any(|repair| repair.origins.is_some()) {
        return;
    }
    let supplied = migrations
        .iter()
        .map(|migration| {
            let not_null = migration
                .columns
                .iter()
                .filter(|column| column.not_null && !column.not_null_no_inherit)
                .map(|column| column.name.clone())
                .collect::<BTreeSet<_>>();
            let checks = migration
                .constraints
                .checks
                .iter()
                .filter(|check| !check.no_inherit)
                .filter_map(|check| check.name.clone())
                .chain(
                    migration
                        .columns
                        .iter()
                        .filter(|column| column.check.is_some() && !column.check_no_inherit)
                        .filter_map(|column| column.check_name.clone()),
                )
                .collect::<BTreeSet<_>>();
            (
                migration.schema.relation.qualified_name(),
                (not_null, checks),
            )
        })
        .collect::<BTreeMap<_, _>>();
    for (migration, repair) in migrations.iter_mut().zip(repairs) {
        let Some(origins) = repair.origins else {
            continue;
        };
        let mut not_null = BTreeSet::new();
        let mut checks = BTreeSet::new();
        for parent in &migration.constraints.hierarchy.parents {
            if let Some((parent_not_null, parent_checks)) = supplied.get(parent) {
                not_null.extend(parent_not_null.iter().cloned());
                checks.extend(parent_checks.iter().cloned());
            }
        }
        origins.update_not_null(&mut migration.columns, &not_null);
        origins.update_checks(
            &mut migration.columns,
            &mut migration.constraints.checks,
            &checks,
        );
    }
}

fn split_detached_tree(
    migrations: &mut [ConstraintMetadataMigration],
    root: usize,
    repair: &restoration::ParentEdgeRepair,
) -> StorageBackendResult<()> {
    let mut families = BTreeMap::new();
    for key in metadata_foreign_keys(&migrations[root].columns, &migrations[root].constraints) {
        if let Some(id) = key.object_id {
            if let std::collections::btree_map::Entry::Vacant(entry) = families.entry(id) {
                entry.insert(crate::catalog::identity::new_nonzero_catalog_identity(
                    &migrations[root].schema.relation.qualified_name(),
                    "restored foreign-key family",
                )?);
            }
        }
    }
    let descendants = partition_descendants(migrations, root);
    for (index, migration) in migrations.iter_mut().enumerate() {
        let table = migration.schema.relation.qualified_name();
        if !descendants.contains(&table) {
            continue;
        }
        if index != root {
            restoration::restore_partition_identity(
                &mut migration.columns,
                &mut migration.constraints,
                &repair.inherited_identity,
            );
        }
        split_foreign_key_families(
            &table,
            &mut migration.columns,
            &mut migration.constraints,
            &families,
        )
        .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        migration.changed = true;
    }
    Ok(())
}

fn partition_descendants(
    migrations: &[ConstraintMetadataMigration],
    root: usize,
) -> BTreeSet<String> {
    let mut descendants = BTreeSet::from([migrations[root].schema.relation.qualified_name()]);
    loop {
        let previous = descendants.len();
        for migration in migrations {
            if migration.constraints.hierarchy.is_partition()
                && migration
                    .constraints
                    .hierarchy
                    .parents
                    .iter()
                    .any(|parent| descendants.contains(parent))
            {
                descendants.insert(migration.schema.relation.qualified_name());
            }
        }
        if descendants.len() == previous {
            break;
        }
    }
    descendants
}
