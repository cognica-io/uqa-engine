//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table ownership, document mutations, and retained views through real Key/Value consumers.

use crate::document_store::identifiers::DocumentIdAllocator;
use crate::{
    CatalogFacade, DocumentMetadata, DocumentStore, KeyValueCatalog, KeyValueDocumentStore,
    KeyValueStore, RelationIdentity, StorageBackendResult, StoredDocument, TableSchema,
};
use std::collections::BTreeMap;
use std::sync::Arc;
use uqa_core::Value;

fn schema(name: &str) -> TableSchema {
    TableSchema {
        relation: RelationIdentity::new("public", name),
        role_owner: "owner".into(),
        acl: None,
        column_acls: BTreeMap::new(),
        object_id: [0; 16],
        storage_generation: [0; 16],
        analyzer_json: "{}".into(),
        fts_fields: vec![],
        vector_fields: vec![],
        columns_json: "[]".into(),
        constraints_json: "{}".into(),
    }
}

fn fields(n: i64) -> BTreeMap<String, Value> {
    BTreeMap::from([("n".into(), Value::Int(n))])
}

fn loaded(catalog: &KeyValueCatalog, name: &str) -> StorageBackendResult<TableSchema> {
    Ok(catalog
        .load_tables()?
        .into_iter()
        .find(|table| table.relation.name == name)
        .expect("saved table"))
}

fn allocate(store: &dyn KeyValueStore, schema: &TableSchema) -> StorageBackendResult<u64> {
    DocumentIdAllocator::new(
        store.identifier_allocator(),
        schema.object_id,
        schema.storage_generation,
    )?
    .allocate(&mut 1)
}

/// Exercise two independent sessions and leave `public.owner_renamed` with a reserved watermark of 105 for closed-file verification.
pub fn verify_document_ownership(
    a: &Arc<dyn KeyValueStore>,
    b: &Arc<dyn KeyValueStore>,
) -> StorageBackendResult<()> {
    let catalog = KeyValueCatalog::new(a.clone());
    let other = KeyValueCatalog::new(b.clone());
    catalog.save_schema("public")?;
    catalog.save_table(&schema("owner_docs"))?;
    let original = loaded(&catalog, "owner_docs")?;
    assert_ne!(original.object_id, [0; 16]);
    assert_ne!(original.storage_generation, [0; 16]);
    let mut first = KeyValueDocumentStore::new(a.clone(), "public.owner_docs");
    let mut second = KeyValueDocumentStore::new(b.clone(), "public.owner_docs");
    first.put_stored(
        100,
        StoredDocument::with_metadata(fields(1), DocumentMetadata::with_tuple_xmin(71)),
    )?;
    first.put(100, fields(2))?;
    assert_eq!(first.get_metadata(100)?.unwrap().tuple_xmin(), Some(71));
    first.patch_fields(
        100,
        &BTreeMap::from([("n".into(), Value::Null), ("kept".into(), Value::Int(3))]),
    )?;
    assert!(first.get_field(100, "n")?.is_none());
    assert_eq!(first.get_metadata(100)?.unwrap().tuple_xmin(), Some(71));
    let retained = first.snapshot()?;
    first.delete(100)?;
    assert_eq!(allocate(&**b, &original)?, 101);
    assert!(retained.contains_doc_id(100)?);

    a.begin_transaction()?;
    b.begin_transaction()?;
    first.put(1, fields(1))?;
    second.put(2, fields(2))?;
    b.commit_transaction()?;
    a.commit_transaction()?;
    assert_eq!(first.doc_ids()?, vec![1, 2]);
    for structure_first in [false, true] {
        a.begin_transaction()?;
        b.begin_transaction()?;
        first.put(3, fields(3))?;
        let mut changed = original.clone();
        changed.role_owner = "changed".into();
        other.save_table(&changed)?;
        if structure_first {
            b.commit_transaction()?;
            assert!(a.commit_transaction().is_err());
            a.rollback_transaction()?;
        } else {
            a.commit_transaction()?;
            assert!(b.commit_transaction().is_err());
            b.rollback_transaction()?;
        }
    }

    catalog.purge_table_data("public.owner_docs")?;
    assert!(first.is_empty()?);
    assert_eq!(allocate(&**b, &original)?, 102);
    catalog.drop_table("owner_docs")?;
    catalog.save_table(&schema("owner_docs"))?;
    let adopted = loaded(&catalog, "owner_docs")?;
    assert_eq!(adopted.object_id, original.object_id);
    assert_eq!(adopted.storage_generation, original.storage_generation);
    catalog.rename_table_data("public.owner_docs", "public.owner_renamed")?;
    let mut renamed = loaded(&catalog, "owner_renamed")?;
    assert_eq!(renamed.object_id, original.object_id);
    renamed.object_id = [17; 16];
    renamed.storage_generation = [18; 16];
    catalog.save_table(&renamed)?;
    assert_eq!(allocate(&**b, &renamed)?, 103);
    a.begin_transaction()?;
    a.savepoint("documents")?;
    KeyValueDocumentStore::new(a.clone(), "public.owner_renamed").put(104, fields(104))?;
    a.rollback_to_savepoint("documents")?;
    a.commit_transaction()?;
    assert!(
        KeyValueDocumentStore::new(b.clone(), "public.owner_renamed")
            .get(104)?
            .is_none()
    );

    let mut raw = KeyValueDocumentStore::new(a.clone(), "owner_renamed");
    raw.put(1000, fields(1000))?;
    raw.delete(1000)?;
    assert_eq!(allocate(&**b, &renamed)?, 105);
    // Leave a deterministic reserved floor even after no live row retains its high identity.
    assert!(retained.get(100)?.is_some());
    assert!(retained.snapshot()?.get(100)?.is_some());
    let mut snapshot = retained.snapshot()?;
    assert!(Arc::get_mut(&mut snapshot).unwrap().clear().is_err());
    Ok(())
}

/// Verify the owner and independent watermark after all original handles have closed.
pub fn verify_document_reopen(store: &Arc<dyn KeyValueStore>) -> StorageBackendResult<()> {
    let catalog = KeyValueCatalog::new(store.clone());
    let saved = loaded(&catalog, "owner_renamed")?;
    assert_eq!(
        (saved.object_id, saved.storage_generation),
        ([17; 16], [18; 16])
    );
    assert_eq!(allocate(&**store, &saved)?, 106);
    assert!(KeyValueDocumentStore::new(store.clone(), "public.owner_renamed").is_empty()?);
    catalog.drop_table_and_data("owner_renamed")?;
    catalog.save_table(&schema("owner_renamed"))?;
    let recreated = loaded(&catalog, "owner_renamed")?;
    assert_ne!(recreated.object_id, saved.object_id);
    assert_ne!(recreated.storage_generation, saved.storage_generation);
    assert_eq!(allocate(&**store, &recreated)?, 1);
    Ok(())
}
