//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Coordinate the complete ordinary and foreign constraint candidate before writing converted rows.

use super::{ConstraintMetadataMigration, CATALOG_ADDRESS_METADATA_KEY};
use std::collections::{BTreeMap, BTreeSet};
use uqa_sql::ast::{ColumnDef, ConstraintCatalogIdentity, TableConstraintSet};
use uqa_sql::schema::constraint_metadata::identity::{claims, foreign_keys};
use uqa_storage::{CatalogFacade, ForeignTableRow, StorageBackendError, StorageBackendResult};

#[derive(Default)]
pub(super) struct MigrationAllocator(
    BTreeSet<(uqa_sql::schema::constraint_metadata::CatalogOidClass, i64)>,
);

impl uqa_sql::schema::constraint_metadata::CatalogObjectAllocator for MigrationAllocator {
    fn include_catalog_identity(
        &mut self,
        _relation: &uqa_core::RelationIdentity,
        class: uqa_sql::schema::constraint_metadata::CatalogOidClass,
        identity: ConstraintCatalogIdentity,
    ) -> uqa_sql::schema::constraint_metadata::ConstraintMetadataResult<()> {
        self.0.insert((class, identity.oid));
        Ok(())
    }

    fn allocate_object_id(
        &mut self,
        kind: &str,
    ) -> uqa_sql::schema::constraint_metadata::ConstraintMetadataResult<[u8; 16]> {
        crate::catalog::identity::allocate_catalog_object_id(kind)
    }

    fn allocate_catalog_oid(
        &mut self,
        class: uqa_sql::schema::constraint_metadata::CatalogOidClass,
        object_id: &[u8; 16],
    ) -> uqa_sql::schema::constraint_metadata::ConstraintMetadataResult<i64> {
        let mut oid = uqa_sql::catalog::oids::stable_object_oid(class.label(), object_id);
        loop {
            if oid >= 16_384 && self.0.insert((class, oid)) {
                return Ok(oid);
            }
            oid =
                crate::catalog::identity::allocate_catalog_oid(class.label()).map_err(|error| {
                    uqa_sql::schema::constraint_metadata::ConstraintMetadataError::Execution(
                        Box::new(error),
                    )
                })?;
        }
    }
}

pub(super) fn require_format(
    catalog: &dyn CatalogFacade,
    allow_legacy: bool,
) -> StorageBackendResult<bool> {
    match catalog
        .get_metadata(CATALOG_ADDRESS_METADATA_KEY)?
        .as_deref()
    {
        Some("1") => Ok(false),
        None if allow_legacy => Ok(true),
        None => Err(StorageBackendError::Other(
            "constraints require an initial catalog address migration".into(),
        )),
        Some(_) => Err(StorageBackendError::Other(
            "unknown constraint catalog address format".into(),
        )),
    }
}

pub(super) struct ForeignMigration {
    pub row: ForeignTableRow,
    pub table: crate::catalog::foreign::StoredForeignTable,
    pub changed: bool,
    pub legacy_addresses: BTreeSet<[u8; 16]>,
}

pub(super) fn key_and_check_identities(
    columns: &[ColumnDef],
    constraints: &TableConstraintSet,
) -> Vec<ConstraintCatalogIdentity> {
    constraints
        .key_constraints
        .iter()
        .filter_map(|key| key.catalog_identity)
        .chain(columns.iter().filter_map(|column| {
            column
                .check_object_id
                .zip(column.check_catalog_oid)
                .map(|(object_id, oid)| ConstraintCatalogIdentity { object_id, oid })
        }))
        .chain(constraints.checks.iter().filter_map(|check| {
            check
                .object_id
                .zip(check.catalog_oid)
                .map(|(object_id, oid)| ConstraintCatalogIdentity { object_id, oid })
        }))
        .collect()
}

fn identified_rows(
    columns: &[ColumnDef],
    constraints: &TableConstraintSet,
) -> Vec<(ConstraintCatalogIdentity, &'static str)> {
    columns
        .iter()
        .filter_map(|column| column.not_null_identity)
        .map(|identity| (identity, "NOT NULL constraint"))
        .chain(
            foreign_keys::identities(columns, constraints)
                .flatten()
                .map(|identity| (identity, "foreign-key")),
        )
        .chain(
            key_and_check_identities(columns, constraints)
                .into_iter()
                .map(|identity| (identity, "key or CHECK constraint")),
        )
        .collect()
}

pub(super) fn coordinate(
    tables: &mut [ConstraintMetadataMigration],
    foreign: &mut [ForeignMigration],
) -> StorageBackendResult<()> {
    let mut rows = Vec::new();
    for table in tables.iter() {
        rows.extend(
            identified_rows(&table.columns, &table.constraints)
                .into_iter()
                .map(|(identity, kind)| {
                    (
                        identity,
                        kind,
                        table.legacy_addresses.contains(&identity.object_id),
                    )
                }),
        );
    }
    for table in foreign.iter() {
        let constraints = TableConstraintSet {
            checks: table.table.checks.clone(),
            ..Default::default()
        };
        rows.extend(
            identified_rows(&table.table.columns, &constraints)
                .into_iter()
                .map(|(identity, kind)| {
                    (
                        identity,
                        kind,
                        table.legacy_addresses.contains(&identity.object_id),
                    )
                }),
        );
    }
    let replacements = coordinate_addresses(&rows)?;
    let mut identities = BTreeSet::new();
    let mut oids = BTreeSet::new();
    for table in tables {
        table.changed |=
            apply_replacements(&mut table.columns, &mut table.constraints, &replacements);
        validate(
            &table.columns,
            &table.constraints,
            &mut identities,
            &mut oids,
        )?;
    }
    for table in foreign {
        let mut constraints = TableConstraintSet {
            checks: std::mem::take(&mut table.table.checks),
            ..Default::default()
        };
        table.changed |=
            apply_replacements(&mut table.table.columns, &mut constraints, &replacements);
        validate(
            &table.table.columns,
            &constraints,
            &mut identities,
            &mut oids,
        )?;
        table.table.checks = constraints.checks;
    }
    Ok(())
}

fn coordinate_addresses(
    rows: &[(ConstraintCatalogIdentity, &'static str, bool)],
) -> StorageBackendResult<BTreeMap<[u8; 16], i64>> {
    let mut incarnations = BTreeSet::new();
    let mut claimed = BTreeSet::new();
    for (identity, kind, legacy) in rows {
        if !identity.is_valid() {
            return Err(StorageBackendError::Other(format!(
                "invalid {kind} catalog identity"
            )));
        }
        if !incarnations.insert(identity.object_id) || (!legacy && !claimed.insert(identity.oid)) {
            return Err(StorageBackendError::Other(format!(
                "duplicate {kind} catalog identity"
            )));
        }
    }
    let collisions: Vec<_> = rows
        .iter()
        .filter(|(identity, _, legacy)| *legacy && !claimed.insert(identity.oid))
        .collect();
    let mut replacements = BTreeMap::new();
    for (identity, _, _) in collisions {
        loop {
            let oid = crate::catalog::identity::allocate_catalog_oid("constraint")
                .map_err(|error| StorageBackendError::backend("constraint identity", error))?;
            if claimed.insert(oid) {
                replacements.insert(identity.object_id, oid);
                break;
            }
        }
    }
    Ok(replacements)
}

fn apply_replacements(
    columns: &mut [ColumnDef],
    constraints: &mut TableConstraintSet,
    replacements: &BTreeMap<[u8; 16], i64>,
) -> bool {
    let mut changed = false;
    let replace = |object: Option<[u8; 16]>, oid: &mut Option<i64>, changed: &mut bool| {
        if let Some(replacement) = object.and_then(|object| replacements.get(&object)) {
            *oid = Some(*replacement);
            *changed = true;
        }
    };
    let replace_identity = |identity: &mut Option<ConstraintCatalogIdentity>,
                            changed: &mut bool| {
        if let Some(identity) = identity {
            if let Some(replacement) = replacements.get(&identity.object_id) {
                identity.oid = *replacement;
                *changed = true;
            }
        }
    };
    for column in columns {
        replace_identity(&mut column.not_null_identity, &mut changed);
        if let Some(reference) = &mut column.references {
            replace_identity(&mut reference.catalog_identity, &mut changed);
        }
        replace(
            column.check_object_id,
            &mut column.check_catalog_oid,
            &mut changed,
        );
    }
    for key in &mut constraints.key_constraints {
        replace_identity(&mut key.catalog_identity, &mut changed);
    }
    for key in &mut constraints.foreign_keys {
        replace_identity(&mut key.catalog_identity, &mut changed);
    }
    for key in &mut constraints.hierarchy.partition_inherited_key_constraints {
        replace_identity(&mut key.catalog_identity, &mut changed);
    }
    for key in &mut constraints.hierarchy.partition_inherited_foreign_keys {
        replace_identity(&mut key.catalog_identity, &mut changed);
    }
    for check in &mut constraints.checks {
        replace(check.object_id, &mut check.catalog_oid, &mut changed);
    }
    changed |=
        uqa_sql::schema::constraint_metadata::synchronize_partition_inherited_foreign_key_ids(
            constraints,
        );
    changed |=
        uqa_sql::schema::constraint_metadata::identity::keys::synchronize_provenance(constraints);
    changed
}

fn validate(
    columns: &[ColumnDef],
    constraints: &TableConstraintSet,
    identities: &mut BTreeSet<[u8; 16]>,
    oids: &mut BTreeSet<i64>,
) -> StorageBackendResult<()> {
    claims::validate_constraint_identities(columns, constraints)
        .map_err(|error| StorageBackendError::backend("constraint identity", error))?;
    super::register_identities(columns, constraints, identities, oids)
}

#[cfg(test)]
mod tests;
