//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::diskann_index::catalog::DiskANNIndexResolver;
use uqa_storage::key_value::{
    conformance::build_diskann_publication_fixture, publication::selected_generation,
    KeyValueDiskANNCanonical, RetainedDiskANNCanonical,
};
use uqa_storage::vector_index::DiskANNIndexParams;
use uqa_storage::{
    CatalogFacade, CatalogIndexRow, KeyValueCatalog, RelationIdentity, RelationSecurityRow,
    StorageBackendResult, TableSchema, VectorFieldSchema,
};

pub(super) struct Resolver;
impl DiskANNIndexResolver for Resolver {
    fn resolve(
        &self,
        definition: &str,
        table: [u8; 16],
        _: &StorageReadControl,
    ) -> StorageBackendResult<[u8; 16]> {
        assert_eq!(definition, "publication fixture");
        assert_eq!(table, [71; 16]);
        Ok([73; 16])
    }
}

pub(super) fn setup(store: &Arc<dyn KeyValueStore>) -> KeyValueDiskANNCanonical {
    let catalog = KeyValueCatalog::new(store.clone());
    catalog.save_schema("public").unwrap();
    catalog
        .save_table(&TableSchema {
            relation: RelationIdentity::new("public", "publication"),
            security: RelationSecurityRow::legacy("owner"),
            object_id: [71; 16],
            storage_generation: [72; 16],
            analyzer_json: "{}".into(),
            fts_fields: vec![],
            vector_fields: vec![VectorFieldSchema {
                field: "vector".into(),
                dimensions: 2,
            }],
            columns_json: "[]".into(),
            constraints_json: "{}".into(),
        })
        .unwrap();
    catalog
        .save_catalog_index_row(&CatalogIndexRow {
            relation: RelationIdentity::new("public", "publication_idx"),
            index_type: "diskann".into(),
            table_name: "public.publication".into(),
            columns_json: "[\"vector\"]".into(),
            parameters_json: serde_json::to_string(
                &DiskANNIndexParams::for_dimensions(2)
                    .unwrap()
                    .to_catalog_map(2)
                    .unwrap(),
            )
            .unwrap(),
            definition_json: Some("publication fixture".into()),
        })
        .unwrap();
    KeyValueDiskANNCanonical::new(store.clone(), "public.publication", "vector", 2).unwrap()
}

#[test]
fn diskann_publication_resolves_lost_replies_without_re_evaluating_the_build() {
    for fault in [
        CommitFault::LoseReply,
        CommitFault::LoseBeforeCommit,
        CommitFault::Reject,
    ] {
        let persistence = Persistence::new();
        let store: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
        let control = StorageReadControl::with_limit(1 << 20);
        let canonical = setup(&store);
        canonical.replace(1, &[vec![1.0, 0.0]], &control).unwrap();
        let source = canonical
            .retain_for_index(
                &RelationIdentity::new("public", "publication_idx"),
                &control,
            )
            .unwrap();
        let parameters = source.index_parameters().unwrap();
        let scope = source.index_scope(&Resolver, &control).unwrap();
        let repository = KeyValueDiskANNStore::connect(&store, &control).unwrap();
        repository.initialize(&control).unwrap();
        let mut stage = repository.allocate_bound_stage(&scope, &control).unwrap();
        let coverage =
            build_diskann_publication_fixture(source, &mut stage, parameters, &control).unwrap();
        let sealed = repository
            .open_source(stage.generation(), &control)
            .unwrap();
        let before = persistence.state.lock().attempts.len();
        persistence.state.lock().commit_fault = fault;
        let mut calls = 0;
        assert!(store
            .with_mutation(&mut |read, batch| {
                calls += 1;
                RetainedDiskANNCanonical::publish_generation(
                    &coverage, &Resolver, &sealed, read, batch, &control,
                )
            })
            .is_err());
        persistence.state.lock().commit_fault = CommitFault::None;
        store.commit_transaction().unwrap();
        assert_eq!(calls, 1);
        let receipts = persistence.state.lock();
        assert_eq!(
            receipts.attempts[before],
            *receipts.attempts.last().unwrap()
        );
        assert_eq!(
            receipts.attempts.len() - before,
            if fault == CommitFault::LoseBeforeCommit {
                2
            } else {
                1
            }
        );
        drop(receipts);
        store
            .with_read_view(&mut |read| {
                assert_eq!(
                    selected_generation(&scope, read, &control)?,
                    Some(stage.generation())
                );
                Ok(())
            })
            .unwrap();
        assert_eq!(
            stage.status(&control).unwrap(),
            Some(DiskANNStageStatus::Published)
        );
    }
}
