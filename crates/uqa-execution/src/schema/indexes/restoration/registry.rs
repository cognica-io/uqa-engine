//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Convert the complete legacy index graph before publishing any converted metadata.

use super::{
    invalid, BTreeMap, BTreeSet, CatalogFacade, CatalogIndexRow, CatalogObjectIdentity,
    CatalogReadView, RelationIdentity, RelationNameResolution, StorageBackendResult, VERSION,
};
use crate::catalog::index::index_definition;
use crate::schema::indexes::registry::{constraints, foreign_keys, partitions, validation};
use uqa_sql::{
    ast::{ColumnDef, TableConstraintSet},
    schema::constraint_metadata::{
        CatalogObjectAllocator, CatalogOidClass, ConstraintMetadataResult,
    },
};

pub(super) use crate::schema::indexes::constraint_names::REGISTRY_VERSION;

#[derive(Debug)]
pub struct RestoredIndexCatalog {
    pub rows: Vec<CatalogIndexRow>,
    pub schemas: Vec<(RelationIdentity, Vec<ColumnDef>, TableConstraintSet)>,
    pub builds: Vec<CatalogIndexRow>,
}

impl std::ops::Deref for RestoredIndexCatalog {
    type Target = [CatalogIndexRow];
    fn deref(&self) -> &Self::Target {
        &self.rows
    }
}

pub fn restore(
    storage: &dyn CatalogFacade,
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    allow_migration: bool,
) -> StorageBackendResult<RestoredIndexCatalog> {
    let names = crate::schema::indexes::constraint_names::KeyConstraintNames::load(storage)?;
    let (projected, refreshed) = names.project(catalog)?;
    let mut durable = projected.snapshot().clone();
    durable
        .tables
        .retain(|_, table| table.persistence != uqa_sql::ast::RelationPersistence::Temporary);
    let catalog = &CatalogReadView::new(durable);
    let (legacy, names_migration) = match storage.get_metadata(REGISTRY_VERSION)?.as_deref() {
        Some("2") => (false, false),
        Some("1") if allow_migration => (false, true),
        None if allow_migration => (true, true),
        _ => {
            return Err(invalid(
                "index registry migration is incomplete or unsupported",
            ))
        }
    };
    let stored = storage.load_catalog_indexes()?;
    let mut rows = super::restore_addresses(storage, catalog, resolution, allow_migration)?
        .into_iter()
        .map(|row| (row.relation.clone(), row))
        .collect::<BTreeMap<_, _>>();
    let (mut candidate, _) =
        super::candidate_catalog(catalog, &rows.values().cloned().collect::<Vec<_>>())?;
    let mut schemas = Vec::new();
    let mut tables = super::tables::Tables::load(storage, &names)?;
    if legacy {
        candidate = tables.bind_foreign_keys(&candidate)?;
        candidate = materialize_legacy(candidate, resolution, &mut rows, &stored, &mut tables)?;
    }
    validation::validate(&candidate, &rows)?;
    let mut schema_rows = Vec::new();
    for (name, (columns, constraints)) in &mut tables.declarations {
        let persist = foreign_keys::bind(&rows, columns, constraints, legacy)?
            || tables.changed.contains(name)
            || names_migration;
        if persist || refreshed.contains(name) {
            schemas.push((name.clone(), columns.clone(), constraints.clone()));
        }
        if persist {
            let mut table = tables.rows[name].clone();
            table.columns_json = serde_json::to_string(columns)?;
            table.constraints_json = crate::schema::indexes::constraint_names::encode(constraints)?;
            schema_rows.push(table);
        }
    }
    // No writes occur until addresses, table ownership, complete ancestry and every FK reference have passed validation.
    for row in rows.values() {
        if stored
            .iter()
            .find(|old| old.relation == row.relation)
            .is_none_or(|old| old.definition_json != row.definition_json)
        {
            storage.save_catalog_index_row(row)?;
        }
    }
    for table in schema_rows {
        storage.save_table(&table)?;
    }
    if storage.get_metadata(VERSION)?.is_none() {
        storage.set_metadata(VERSION, "1")?;
    }
    if names_migration {
        storage.set_metadata(REGISTRY_VERSION, "2")?;
    }
    let builds = if legacy {
        crate::schema::indexes::registry::builds::new_physical_indexes(
            &stored
                .into_iter()
                .map(|row| (row.relation.clone(), row))
                .collect(),
            &rows,
        )?
    } else {
        Vec::new()
    };
    Ok(RestoredIndexCatalog {
        builds,
        rows: rows.into_values().collect(),
        schemas,
    })
}

struct MigrationAllocator {
    occupied: BTreeSet<(CatalogOidClass, i64)>,
}

impl MigrationAllocator {
    fn new(
        catalog: &CatalogReadView,
        resolution: &RelationNameResolution,
        rows: &BTreeMap<RelationIdentity, CatalogIndexRow>,
    ) -> StorageBackendResult<Self> {
        let mut empty = catalog.snapshot().clone();
        empty.definitions.catalog_indexes = BTreeMap::new().into();
        for table in empty.tables.values_mut() {
            table.keys = Vec::new().into();
        }
        let mut occupied = crate::catalog::projection::legacy_relation_claims(
            &CatalogReadView::new(empty),
            resolution,
        )
        .map_err(invalid)?
        .into_iter()
        .map(|claim| (CatalogOidClass::Relation, claim.oid))
        .collect::<BTreeSet<_>>();
        for row in rows.values() {
            let identity = index_definition(row)?
                .catalog
                .ok_or_else(|| invalid("stored index has no address"))?;
            occupied.insert((CatalogOidClass::Relation, identity.identity.oid));
        }
        Ok(Self { occupied })
    }
}

impl CatalogObjectAllocator for MigrationAllocator {
    fn include_catalog_identity(
        &mut self,
        _: &RelationIdentity,
        class: CatalogOidClass,
        identity: CatalogObjectIdentity,
    ) -> ConstraintMetadataResult<()> {
        self.occupied.insert((class, identity.oid));
        Ok(())
    }
    fn allocate_object_id(&mut self, kind: &str) -> ConstraintMetadataResult<[u8; 16]> {
        crate::catalog::identity::allocate_catalog_object_id(kind)
    }
    fn allocate_catalog_oid(
        &mut self,
        class: CatalogOidClass,
        object: &[u8; 16],
    ) -> ConstraintMetadataResult<i64> {
        let mut oid = uqa_sql::catalog::oids::stable_object_oid(class.label(), object);
        loop {
            if oid >= 16384 && self.occupied.insert((class, oid)) {
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

fn materialize_legacy(
    mut candidate: CatalogReadView,
    resolution: &RelationNameResolution,
    rows: &mut BTreeMap<RelationIdentity, CatalogIndexRow>,
    stored: &[CatalogIndexRow],
    tables: &mut super::tables::Tables,
) -> StorageBackendResult<CatalogReadView> {
    let predecessor = crate::catalog::projection::legacy_index_relations(&candidate, resolution)
        .map_err(invalid)?;
    let mut allocator = MigrationAllocator::new(&candidate, resolution, rows)?;
    candidate = tables.prepare(&candidate, &mut allocator)?;
    for (relation, table) in &candidate.snapshot().tables {
        let constraints = TableConstraintSet {
            key_constraints: table.keys.as_ref().clone(),
            ..Default::default()
        };
        let change = constraints::prepare(
            &candidate,
            &relation.qualified_name(),
            table.object_id,
            &table.columns,
            &constraints,
            &mut allocator,
        )?;
        for row in change.upserts {
            rows.insert(row.relation.clone(), row);
        }
    }
    super::legacy::preserve_derived_names(&candidate, rows, stored, &predecessor, &mut allocator)?;
    partitions::materialize(&candidate, rows, &mut allocator)?;
    for row in rows
        .values_mut()
        .filter(|row| !stored.iter().any(|old| old.relation == row.relation))
    {
        if let Some(old) = predecessor.iter().find(|old| old.relation == row.relation) {
            let mut definition = index_definition(row)?;
            let identity = definition.catalog.as_mut().expect("converted address");
            if allocator
                .occupied
                .insert((CatalogOidClass::Relation, old.oid()))
            {
                allocator
                    .occupied
                    .remove(&(CatalogOidClass::Relation, identity.identity.oid));
                identity.identity.oid = old.oid();
            }
            row.definition_json = Some(serde_json::to_string(&definition)?);
        }
    }
    Ok(candidate)
}
