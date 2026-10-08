//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::cell::RefCell;
use uqa_storage::StorageBackendResult;

#[derive(Default)]
struct Publication {
    persisted: RefCell<Vec<CatalogIndexRow>>,
    published: RefCell<Vec<CatalogIndexRow>>,
    removed: RefCell<Vec<RelationIdentity>>,
    forgotten: RefCell<Vec<RelationIdentity>>,
}

impl IndexRegistryPublication for Publication {
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

    fn publish_index(&self, row: CatalogIndexRow) {
        self.published.borrow_mut().push(row);
    }

    fn forget_index(&self, relation: &RelationIdentity) {
        self.forgotten.borrow_mut().push(relation.clone());
    }

    fn refresh_index_table(&self, _: &str) -> StorageBackendResult<()> {
        panic!("a retained namespace move must not reload the partly moved catalog")
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
    PreparedIndexRelocations {
        rows: vec![(previous.clone(), renamed)],
    }
    .publish(&publication)
    .unwrap();
    assert_eq!(
        publication.removed.borrow().as_slice(),
        std::slice::from_ref(&previous.relation)
    );
    assert_eq!(*publication.forgotten.borrow(), [previous.relation]);
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
