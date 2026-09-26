//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::diskann_index::catalog::DiskANNIndexResolver;
use uqa_storage::StorageBackendResult;

struct Resolver;

impl DiskANNIndexResolver for Resolver {
    fn resolve(
        &self,
        definition: &str,
        table: [u8; 16],
        control: &StorageReadControl,
    ) -> StorageBackendResult<[u8; 16]> {
        control.check()?;
        assert_eq!(table, [71; 16]);
        serde_json::from_str(definition).map_err(Into::into)
    }
}

#[test]
fn native_diskann_catalog_identity_keeps_actual_private_definitions_and_durable_handles() {
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("identity.db");
        let generation = {
            let connection = open(&path, mode);
            let catalog = setup(&connection);
            let mut row = row();
            row.definition_json = Some(serde_json::to_string(&[82; 16]).unwrap());
            catalog.save_catalog_index_row(&row).unwrap();
            let original = StorageReadControl::with_limit(1 << 20);
            let query = StorageReadControl::with_limit(1 << 20);
            let source = capture(&connection, &original);
            let scope = source.index_scope(&Resolver, &query).unwrap();
            assert_eq!(scope.table_object(), [71; 16]);
            assert_eq!(scope.storage_generation(), [72; 16]);
            assert_eq!(scope.index_object(), [82; 16]);
            let repository = connection.diskann_generations(&query).unwrap();
            repository.initialize(&query).unwrap();
            let mut stage = repository.allocate_bound_stage(&scope, &query).unwrap();
            stage.start(&query).unwrap();
            let generation = stage.generation();
            let foreign = open(&directory.path().join("foreign.db"), mode);
            let foreign_repository = foreign.diskann_generations(&query).unwrap();
            foreign_repository.initialize(&query).unwrap();
            assert!(foreign_repository
                .allocate_bound_stage(&scope, &query)
                .is_err());
            connection.begin_transaction().unwrap();
            row.definition_json = Some(serde_json::to_string(&[83; 16]).unwrap());
            catalog.save_catalog_index_row(&row).unwrap();
            let private = capture(&connection, &query);
            let private_scope = private.index_scope(&Resolver, &query).unwrap();
            let private_stage = repository
                .allocate_bound_stage(&private_scope, &query)
                .unwrap();
            assert_ne!(private_stage.generation().index(), generation.index());
            assert!(connection.in_transaction());
            connection.rollback_transaction().unwrap();
            assert_eq!(
                private
                    .index_scope(&Resolver, &query)
                    .unwrap()
                    .index_object(),
                [83; 16]
            );
            assert!(guard(&connection, &private, &query).is_err());
            assert_eq!(
                source
                    .index_scope(&Resolver, &query)
                    .unwrap()
                    .index_object(),
                [82; 16]
            );
            original.cancellation().cancel();
            assert!(repository.allocate_bound_stage(&scope, &query).is_err());
            generation
        };
        let connection = open(&path, mode);
        let control = StorageReadControl::with_limit(1 << 20);
        let source = capture(&connection, &control);
        let scope = source.index_scope(&Resolver, &control).unwrap();
        let repository = connection.diskann_generations(&control).unwrap();
        let next = repository
            .allocate_bound_stage(&scope, &control)
            .unwrap()
            .generation();
        assert_eq!(
            (next.database(), next.table(), next.index()),
            (
                generation.database(),
                generation.table(),
                generation.index()
            )
        );
        assert!(next.generation() > generation.generation());
    }
}
