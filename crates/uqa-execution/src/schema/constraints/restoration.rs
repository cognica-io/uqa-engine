//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Migrate constraint identities once in the initial catalog transaction and validate later restores.

use std::collections::{BTreeMap, BTreeSet};
use uqa_sql::schema::constraint_metadata::identity::{
    foreign_keys, migrate_constraint_metadata, validate_not_null_identities,
};
use uqa_storage::{CatalogFacade, StorageBackendError, StorageBackendResult, TableSchema};

pub const IDENTITY_METADATA_KEY: &str = "sql_not_null_constraint_identity_version";
pub const FOREIGN_KEY_IDENTITY_METADATA_KEY: &str = "sql_foreign_key_catalog_identity_version";

/// Validate a load-only catalog without allocating identities or publishing repairs.
pub fn validate_constraint_catalog(catalog: &dyn CatalogFacade) -> StorageBackendResult<()> {
    match catalog.get_metadata(IDENTITY_METADATA_KEY)?.as_deref() {
        Some("1") => {}
        None => {
            return Err(StorageBackendError::Other(
                "NOT NULL constraints require an initial catalog identity migration".into(),
            ))
        }
        Some(_) => {
            return Err(StorageBackendError::Other(
                "unknown NOT NULL constraint identity format".into(),
            ))
        }
    }
    require_foreign_key_identity_format(catalog, false)?;
    let mut identities = BTreeSet::new();
    let mut oids = BTreeSet::new();
    let mut validate = |columns: &[uqa_sql::ast::ColumnDef],
                        constraints: &uqa_sql::ast::TableConstraintSet| {
        validate_not_null_identities(columns)
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        foreign_keys::validate(columns, constraints)
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        register_identities(columns, constraints, &mut identities, &mut oids)
    };
    for row in catalog.load_tables()? {
        let columns = if row.columns_json.is_empty() {
            Vec::new()
        } else {
            serde_json::from_str(&row.columns_json)?
        };
        let constraints = if row.constraints_json.is_empty() {
            uqa_sql::ast::TableConstraintSet::default()
        } else {
            serde_json::from_str(&row.constraints_json)?
        };
        validate(&columns, &constraints)?;
    }
    for row in catalog.load_foreign_tables()? {
        let (table, _) = crate::catalog::foreign::StoredForeignTable::from_catalog(
            row.relation.qualified_name(),
            row.server_name,
            serde_json::from_str(&row.options_json)?,
            &row.columns_json,
        )?;
        validate(&table.columns, &uqa_sql::ast::TableConstraintSet::default())?;
    }
    Ok(())
}

pub fn migrate_constraint_catalog(catalog: &dyn CatalogFacade) -> StorageBackendResult<()> {
    let legacy = match catalog.get_metadata(IDENTITY_METADATA_KEY)?.as_deref() {
        None => true,
        Some("1") => false,
        Some(_) => {
            return Err(StorageBackendError::Other(
                "unknown NOT NULL constraint identity format".into(),
            ))
        }
    };
    let foreign_legacy = require_foreign_key_identity_format(catalog, true)?;
    let mut migrations = load_constraint_metadata_migrations(catalog, legacy, foreign_legacy)?;
    synchronize_inherited_constraint_object_ids(&mut migrations);
    let mut identities = BTreeSet::new();
    let mut oids = BTreeSet::new();
    for migration in &migrations {
        register_identities(
            &migration.columns,
            &migration.constraints,
            &mut identities,
            &mut oids,
        )?;
    }
    let mut foreign_migrations = Vec::new();
    for mut row in catalog.load_foreign_tables()? {
        let (mut table, _) = crate::catalog::foreign::StoredForeignTable::from_catalog(
            row.relation.qualified_name(),
            row.server_name.clone(),
            serde_json::from_str(&row.options_json)?,
            &row.columns_json,
        )?;
        let mut constraints = uqa_sql::ast::TableConstraintSet {
            checks: std::mem::take(&mut table.checks),
            ..Default::default()
        };
        let changed = materialize_metadata(
            &row.relation,
            &mut table.columns,
            &mut constraints,
            legacy,
            foreign_legacy,
        )?;
        register_identities(&table.columns, &constraints, &mut identities, &mut oids)?;
        table.checks = constraints.checks;
        if changed {
            row.columns_json = table.schema_json()?;
            foreign_migrations.push(row);
        }
    }
    save_constraint_metadata_migrations(catalog, migrations)?;
    for row in foreign_migrations {
        catalog.save_foreign_table(&row)?;
    }
    if legacy {
        catalog.set_metadata(IDENTITY_METADATA_KEY, "1")?;
    }
    if foreign_legacy {
        catalog.set_metadata(FOREIGN_KEY_IDENTITY_METADATA_KEY, "1")?;
    }
    Ok(())
}

fn materialize_metadata(
    relation: &uqa_core::RelationIdentity,
    columns: &mut [uqa_sql::ast::ColumnDef],
    constraints: &mut uqa_sql::ast::TableConstraintSet,
    legacy: bool,
    foreign_legacy: bool,
) -> StorageBackendResult<bool> {
    let foreign_migration = if foreign_legacy {
        Some(foreign_keys::LegacyIdentities::capture(
            columns,
            constraints,
        ))
    } else {
        foreign_keys::validate(columns, constraints)
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        None
    };
    let allocate = &mut crate::catalog::identity::allocate_catalog_object_id;
    let mut changed = if legacy {
        migrate_constraint_metadata(relation, columns, constraints, allocate)
    } else {
        validate_not_null_identities(columns)
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        uqa_sql::schema::constraint_metadata::materialize_constraint_metadata(
            relation,
            columns,
            constraints,
            allocate,
        )
    }
    .map_err(|error| StorageBackendError::Other(error.to_string()))?;
    if let Some(migration) = foreign_migration {
        changed |= migration
            .preserve_oids(relation, columns, constraints)
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
    }
    Ok(changed)
}

fn require_foreign_key_identity_format(
    catalog: &dyn CatalogFacade,
    allow_legacy: bool,
) -> StorageBackendResult<bool> {
    match catalog
        .get_metadata(FOREIGN_KEY_IDENTITY_METADATA_KEY)?
        .as_deref()
    {
        Some("1") => Ok(false),
        None if allow_legacy => Ok(true),
        None => Err(StorageBackendError::Other(
            "foreign keys require an initial catalog identity migration".into(),
        )),
        Some(_) => Err(StorageBackendError::Other(
            "unknown foreign-key catalog identity format".into(),
        )),
    }
}

fn register_identities(
    columns: &[uqa_sql::ast::ColumnDef],
    constraints: &uqa_sql::ast::TableConstraintSet,
    identities: &mut BTreeSet<[u8; 16]>,
    oids: &mut BTreeSet<i64>,
) -> StorageBackendResult<()> {
    for identity in columns.iter().filter_map(|column| column.not_null_identity) {
        if !identities.insert(identity.object_id) || !oids.insert(identity.oid) {
            return Err(StorageBackendError::Other(
                "duplicate NOT NULL constraint catalog identity".into(),
            ));
        }
    }
    for identity in foreign_keys::identities(columns, constraints).flatten() {
        if !identities.insert(identity.object_id) || !oids.insert(identity.oid) {
            return Err(StorageBackendError::Other(
                "duplicate foreign-key catalog identity".into(),
            ));
        }
    }
    Ok(())
}

fn metadata_foreign_keys(
    columns: &[uqa_sql::ast::ColumnDef],
    constraints: &uqa_sql::ast::TableConstraintSet,
) -> Vec<uqa_sql::ast::ForeignKey> {
    let mut foreign_keys = constraints.foreign_keys.clone();
    for column in columns {
        let Some(reference) = &column.references else {
            continue;
        };
        foreign_keys.push(uqa_sql::schema::foreign_keys::column_foreign_key(
            column, reference,
        ));
    }
    foreign_keys
}

struct ConstraintMetadataMigration {
    schema: TableSchema,
    columns: Vec<uqa_sql::ast::ColumnDef>,
    constraints: uqa_sql::ast::TableConstraintSet,
    changed: bool,
}

fn load_constraint_metadata_migrations(
    catalog: &dyn CatalogFacade,
    legacy: bool,
    foreign_legacy: bool,
) -> StorageBackendResult<Vec<ConstraintMetadataMigration>> {
    let mut migrations = Vec::new();
    for schema in catalog.load_tables()? {
        let mut columns = if schema.columns_json.is_empty() {
            Vec::new()
        } else {
            serde_json::from_str(&schema.columns_json)?
        };
        let mut constraints = if schema.constraints_json.is_empty() {
            uqa_sql::ast::TableConstraintSet::default()
        } else {
            serde_json::from_str(&schema.constraints_json)?
        };
        let dispatches_changed =
            uqa_sql::schema::dependencies::rewrites::upgrade_legacy_schema_function_dispatches(
                &mut columns,
                &mut constraints,
            );
        let metadata_changed = materialize_metadata(
            &schema.relation,
            &mut columns,
            &mut constraints,
            legacy,
            foreign_legacy,
        )?;
        migrations.push(ConstraintMetadataMigration {
            schema,
            columns,
            constraints,
            changed: dispatches_changed || metadata_changed,
        });
    }
    Ok(migrations)
}

fn inherited_parent_object_id(
    parent_foreign_keys: &BTreeMap<String, Vec<uqa_sql::ast::ForeignKey>>,
    parents: &[String],
    inherited: &uqa_sql::ast::ForeignKey,
) -> Option<[u8; 16]> {
    parents.iter().find_map(|parent| {
        parent_foreign_keys.get(parent).and_then(|foreign_keys| {
            foreign_keys
                .iter()
                .find(|foreign_key| {
                    uqa_sql::schema::constraint_metadata::foreign_keys_match_without_object_id(
                        foreign_key,
                        inherited,
                    )
                })
                .and_then(|foreign_key| foreign_key.object_id)
        })
    })
}

fn apply_inherited_object_id(
    migration: &mut ConstraintMetadataMigration,
    inherited_index: usize,
    object_id: [u8; 16],
) -> bool {
    let inherited = migration
        .constraints
        .hierarchy
        .partition_inherited_foreign_keys[inherited_index]
        .clone();
    let mut changed = false;
    if inherited.object_id != Some(object_id) {
        migration
            .constraints
            .hierarchy
            .partition_inherited_foreign_keys[inherited_index]
            .object_id = Some(object_id);
        changed = true;
    }
    if let Some(foreign_key) = migration
        .constraints
        .foreign_keys
        .iter_mut()
        .find(|foreign_key| {
            uqa_sql::schema::constraint_metadata::foreign_keys_match_without_object_id(
                foreign_key,
                &inherited,
            )
        })
    {
        if foreign_key.object_id != Some(object_id) {
            foreign_key.object_id = Some(object_id);
            changed = true;
        }
    }
    migration.changed |= changed;
    changed
}

fn synchronize_inherited_constraint_object_ids(migrations: &mut [ConstraintMetadataMigration]) {
    for _ in 0..migrations.len() {
        let parent_foreign_keys = migrations
            .iter()
            .map(|migration| {
                (
                    migration.schema.relation.qualified_name(),
                    metadata_foreign_keys(&migration.columns, &migration.constraints),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut pass_changed = false;
        for migration in &mut *migrations {
            let parents = migration.constraints.hierarchy.parents.clone();
            let inherited_count = migration
                .constraints
                .hierarchy
                .partition_inherited_foreign_keys
                .len();
            for inherited_index in 0..inherited_count {
                let inherited = &migration
                    .constraints
                    .hierarchy
                    .partition_inherited_foreign_keys[inherited_index];
                let Some(object_id) =
                    inherited_parent_object_id(&parent_foreign_keys, &parents, inherited)
                else {
                    continue;
                };
                pass_changed |= apply_inherited_object_id(migration, inherited_index, object_id);
            }
        }
        if !pass_changed {
            break;
        }
    }
}

fn save_constraint_metadata_migrations(
    catalog: &dyn CatalogFacade,
    migrations: Vec<ConstraintMetadataMigration>,
) -> StorageBackendResult<()> {
    for mut migration in migrations {
        if !migration.changed {
            continue;
        }
        migration.schema.columns_json = serde_json::to_string(&migration.columns)?;
        migration.schema.constraints_json = serde_json::to_string(&migration.constraints)?;
        catalog.save_table(&migration.schema)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
