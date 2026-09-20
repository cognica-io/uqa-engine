//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain full table declarations while preparing index ownership and FK conversion.

use super::{
    invalid, BTreeMap, BTreeSet, CatalogFacade, CatalogReadView, RelationIdentity,
    StorageBackendError, StorageBackendResult,
};
use uqa_sql::{
    ast::{ColumnDef, TableConstraintSet},
    schema::constraint_metadata::{identity::claims, CatalogObjectAllocator, CatalogOidClass},
};

pub(super) struct Tables {
    pub rows: BTreeMap<RelationIdentity, uqa_storage::TableSchema>,
    pub declarations: BTreeMap<RelationIdentity, (Vec<ColumnDef>, TableConstraintSet)>,
    pub changed: BTreeSet<RelationIdentity>,
}

impl Tables {
    pub fn load(storage: &dyn CatalogFacade) -> StorageBackendResult<Self> {
        let rows = storage
            .load_tables()?
            .into_iter()
            .map(|row| (row.relation.clone(), row))
            .collect::<BTreeMap<_, _>>();
        let mut declarations = BTreeMap::new();
        for (name, row) in &rows {
            let columns = if row.columns_json.is_empty() {
                Vec::new()
            } else {
                serde_json::from_str(&row.columns_json)?
            };
            let constraints = if row.constraints_json.is_empty() {
                TableConstraintSet::default()
            } else {
                serde_json::from_str(&row.constraints_json)?
            };
            declarations.insert(name.clone(), (columns, constraints));
        }
        Ok(Self {
            rows,
            declarations,
            changed: BTreeSet::new(),
        })
    }

    pub fn prepare(
        &mut self,
        catalog: &CatalogReadView,
        allocator: &mut dyn CatalogObjectAllocator,
    ) -> StorageBackendResult<CatalogReadView> {
        for (name, (columns, constraints)) in &self.declarations {
            for identity in claims::row_identities(
                columns,
                &constraints.checks,
                &constraints.key_constraints,
                &constraints.foreign_keys,
            ) {
                allocator
                    .include_catalog_identity(name, CatalogOidClass::Constraint, identity)
                    .map_err(|error| StorageBackendError::backend("index owner address", error))?;
            }
        }
        let mut candidate = catalog.snapshot().clone();
        for (root, _) in catalog
            .snapshot()
            .tables
            .iter()
            .filter(|(_, table)| !table.hierarchy.is_partition())
        {
            let changes = crate::schema::indexes::registry::schema::prepare_descendants(
                catalog,
                &mut candidate,
                root,
                allocator,
                |name| {
                    let (columns, constraints) = self
                        .declarations
                        .get(name)
                        .ok_or_else(|| invalid("partition has no stored declaration"))?;
                    Ok((
                        columns.clone(),
                        constraints.clone(),
                        self.rows[name].object_id,
                    ))
                },
            )?;
            for change in changes {
                self.changed.insert(change.relation.clone());
                self.declarations
                    .insert(change.relation, (change.columns, change.constraints));
            }
        }
        Ok(CatalogReadView::new(candidate))
    }
}
