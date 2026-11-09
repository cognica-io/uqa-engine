//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::index::registry::CatalogIndexes;
use std::cell::RefCell;
use uqa_core::memory::RetainedValue;
use uqa_storage::StorageBackendResult;

#[derive(Default)]
struct Publication {
    persisted: RefCell<Vec<CatalogIndexRow>>,
    published: RefCell<Vec<CatalogIndexRow>>,
    removed: RefCell<Vec<RelationIdentity>>,
    rows: RefCell<CatalogIndexes>,
}

impl IndexRegistryPublication for Publication {
    fn index_catalog(&self) -> RetainedValue<CatalogIndexes> {
        uqa_core::memory::RetainedValue::new(self.rows.borrow().clone())
    }

    fn index_table_persistence(
        &self,
        _: &str,
    ) -> StorageBackendResult<uqa_sql::ast::RelationPersistence> {
        panic!("a retained namespace move preserves native eligibility without reloading tables")
    }
    fn persist_index(
        &self,
        row: &CatalogIndexRow,
        _: crate::schema::indexes::registry::IndexPublicationKind,
    ) -> StorageBackendResult<()> {
        self.persisted.borrow_mut().push(row.clone());
        Ok(())
    }

    fn erase_index(
        &self,
        row: &CatalogIndexRow,
        _: crate::schema::indexes::registry::IndexPublicationKind,
    ) -> StorageBackendResult<()> {
        self.removed.borrow_mut().push(row.relation.clone());
        Ok(())
    }

    fn publish_indexes(&self, rows: CatalogIndexes) {
        self.published.borrow_mut().extend(rows.values().cloned());
        *self.rows.borrow_mut() = rows;
    }

    fn rebind_diskann_index(&self, _: &CatalogIndexRow) -> StorageBackendResult<()> {
        panic!("ordinary index names have no DiskANN handle")
    }

    fn refresh_index_table(&self, _: &str) -> StorageBackendResult<()> {
        panic!("a retained namespace move must not reload the partly moved catalog")
    }

    fn rebind_persistent_vector_field(
        &self,
        _: &str,
        _: &str,
        _: u32,
        _: uqa_storage::VectorIndexSpec,
    ) -> StorageBackendResult<()> {
        panic!("ordinary index names have no vector handle")
    }
}

#[test]
fn prepared_namespace_publication_preserves_index_metadata_without_reloading_tables() {
    let previous = CatalogIndexRow {
        relation: RelationIdentity::new("before", "items_key"),
        index_type: "btree".into(),
        table_name: "before.items".into(),
        columns_json: "[\"id\"]".into(),
        parameters_json: "{}".into(),
        definition_json: Some("retained semantic definition and identity".into()),
    };
    let mut renamed = previous.clone();
    renamed.relation.schema = "after".into();
    renamed.table_name = "after.items".into();
    let publication = Publication::default();
    publication
        .rows
        .borrow_mut()
        .insert(
            previous.clone(),
            uqa_sql::ast::RelationPersistence::Permanent,
        )
        .unwrap();
    PreparedIndexRelocations {
        rows: vec![(previous.clone(), renamed)],
    }
    .publish(&publication)
    .unwrap();
    assert_eq!(
        publication.removed.borrow().as_slice(),
        std::slice::from_ref(&previous.relation)
    );
    assert!(!publication.rows.borrow().contains_key(&previous.relation));
    for rows in [&publication.persisted, &publication.published] {
        let rows = rows.borrow();
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.relation, RelationIdentity::new("after", "items_key"));
        assert_eq!(row.table_name, "after.items");
        assert_eq!(row.index_type, previous.index_type);
        assert_eq!(row.columns_json, previous.columns_json);
        assert_eq!(row.parameters_json, previous.parameters_json);
        assert_eq!(row.definition_json, previous.definition_json);
    }
}

#[test]
fn failed_late_native_index_relocation_admission_precedes_every_persistent_write() {
    let memory = uqa_core::memory::MemoryBudget::new(4 << 20);
    let publication = Publication {
        rows: RefCell::new(CatalogIndexes::with_budget(&memory)),
        ..Publication::default()
    };
    let mut changes = Vec::new();
    for name in ["first", "last"] {
        let row = CatalogIndexRow {
            relation: RelationIdentity::new("before", name),
            table_name: "before.items".into(),
            index_type: "btree".into(),
            columns_json: "[\"id\"]".into(),
            parameters_json: "{}".into(),
            definition_json: None,
        };
        publication
            .rows
            .borrow_mut()
            .insert(row.clone(), uqa_sql::ast::RelationPersistence::Permanent)
            .unwrap();
        let mut renamed = row.clone();
        renamed.relation.schema = "after".into();
        renamed.table_name = "after.items".into();
        if name == "last" {
            renamed.parameters_json = "x".repeat(1 << 20);
        }
        changes.push((row, renamed));
    }
    let before = publication.rows.borrow().clone();
    let used = memory.used();
    let occupied = memory.reserve(memory.available() - (16 << 10)).unwrap();
    let error = PreparedIndexRelocations { rows: changes }
        .publish(&publication)
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert!(publication.removed.borrow().is_empty());
    assert!(publication.persisted.borrow().is_empty());
    assert!(publication.published.borrow().is_empty());
    for (key, row) in before.iter() {
        assert!(std::ptr::eq(
            row,
            publication.rows.borrow().get(key).unwrap()
        ));
    }
    drop(occupied);
    assert_eq!(memory.used(), used);
    drop((before, publication));
    assert_eq!(memory.used(), 0);
}
