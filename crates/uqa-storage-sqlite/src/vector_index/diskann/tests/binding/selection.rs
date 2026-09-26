//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    canonical, capture, identity::Resolver, open, publication::build, row, setup,
    ManagedConnection, RetainedSQLiteDiskANNCanonical, StorageReadControl, FIELD, TABLE,
};
use uqa_storage::diskann_index::{
    format::{DiskANNGeneration, DiskANNVectorVersion, PAGE_BYTES},
    pages::{DiskANNOriginReader, DiskANNPageSource, DiskANNReadLimits},
    DiskANNCanonicalScorer,
};

struct Held {
    view: RetainedSQLiteDiskANNCanonical,
    generation: DiskANNGeneration,
    version: DiskANNVectorVersion,
}

impl Held {
    fn check(&self, control: &StorageReadControl) {
        let source = self
            .view
            .selected_source(&Resolver, control)
            .unwrap()
            .unwrap();
        assert_eq!(source.generation(), self.generation);
        let origins = DiskANNOriginReader::open(source.clone(), 8192, control).unwrap();
        assert_eq!(
            origins.origin(1, control).unwrap().unwrap().version(),
            self.version
        );
        assert_eq!(self.view.origin(1, control).unwrap(), Some(self.version));
        let mut count = 0;
        source
            .read_graph_pages(&[0], control, &mut |id, bytes| {
                assert_eq!(id, 0);
                assert_eq!(bytes.len(), PAGE_BYTES);
                count += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(count, 1);
        let query = self
            .view
            .query(
                &Resolver,
                DiskANNReadLimits {
                    resident_bytes: 65_536,
                    cache_bytes: 0,
                    max_in_flight_page_bytes: 2 * PAGE_BYTES,
                    max_record_bytes: 8192,
                },
                control,
            )
            .unwrap()
            .unwrap();
        let actual = query.search_knn(&[1.0, 0.0], 10, control).unwrap().postings;
        let exact = DiskANNCanonicalScorer::new(&self.view, &[1.0, 0.0], control)
            .unwrap()
            .search_exact_knn(10)
            .unwrap();
        let bits = |postings: &uqa_core::PostingList| {
            postings
                .iter()
                .map(|posting| (posting.doc_id, posting.payload.score.to_bits()))
                .collect::<Vec<_>>()
        };
        assert_eq!(bits(&actual), bits(&exact));
    }
}

#[test]
fn native_diskann_query_views_keep_private_pages_after_undo_and_old_pages_after_publication() {
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("query-views.db");
        let control = StorageReadControl::with_limit(1 << 20);
        let (private, committed, generation) = {
            let connection = open(&path, mode);
            let catalog = setup(&connection);
            let mut definition = row();
            definition.definition_json = Some(serde_json::to_string(&[82; 16]).unwrap());
            catalog.save_catalog_index_row(&definition).unwrap();
            let vectors = canonical(&connection, TABLE, FIELD, 2);
            vectors.replace(1, &[vec![1.0, 0.0]], &control).unwrap();
            vectors.replace(2, &[], &control).unwrap();
            let missing = capture(&connection, &control);
            assert!(missing
                .selected_source(&Resolver, &control)
                .unwrap()
                .is_none());
            let peer = open(&path, mode);
            let private = private_view(&connection, &peer, &control);
            let (committed, generation) = committed_view(&connection, &control);
            assert!(missing
                .selected_source(&Resolver, &control)
                .unwrap()
                .is_none());
            (private, committed, generation)
        };
        private.check(&control);
        committed.check(&control);
        drop((private, committed));
        let connection = open(&path, mode);
        let held = capture(&connection, &control);
        Held {
            version: held.origin(1, &control).unwrap().unwrap(),
            view: held,
            generation,
        }
        .check(&control);
    }
}

fn private_view(
    connection: &ManagedConnection,
    peer: &ManagedConnection,
    control: &StorageReadControl,
) -> Held {
    let vectors = canonical(connection, TABLE, FIELD, 2);
    connection.begin_transaction().unwrap();
    connection.savepoint("before_generation").unwrap();
    let version = vectors
        .replace(1, &[vec![0.0, 1.0], vec![1.0, 0.0]], control)
        .unwrap();
    let (coverage, stage) = build(connection, control);
    connection
        .publish_diskann_generation(&coverage, &Resolver, control)
        .unwrap();
    let generation = stage.generation();
    let held = Held {
        view: capture(connection, control),
        generation,
        version,
    };
    held.check(control);
    connection.savepoint("published_generation").unwrap();
    canonical(peer, TABLE, FIELD, 2)
        .replace(3, &[vec![0.5, 0.5]], control)
        .unwrap();
    assert!(vectors
        .retain(control)
        .unwrap()
        .origin(3, control)
        .unwrap()
        .is_none());
    assert!(capture(peer, control)
        .selected_source(&Resolver, control)
        .unwrap()
        .is_none());
    connection
        .refresh_transaction_snapshot(control.cancellation())
        .unwrap();
    Held {
        view: capture(connection, control),
        generation,
        version,
    }
    .check(control);
    assert!(vectors
        .retain(control)
        .unwrap()
        .origin(3, control)
        .unwrap()
        .is_some());
    connection
        .rollback_to_savepoint("published_generation")
        .unwrap();
    Held {
        view: capture(connection, control),
        generation,
        version,
    }
    .check(control);
    assert!(vectors
        .retain(control)
        .unwrap()
        .origin(3, control)
        .unwrap()
        .is_none());
    connection
        .rollback_to_savepoint("before_generation")
        .unwrap();
    assert!(capture(connection, control)
        .selected_source(&Resolver, control)
        .unwrap()
        .is_none());
    connection.rollback_transaction().unwrap();
    held
}

fn committed_view(
    connection: &ManagedConnection,
    control: &StorageReadControl,
) -> (Held, DiskANNGeneration) {
    let (first, first_stage) = build(connection, control);
    connection
        .publish_diskann_generation(&first, &Resolver, control)
        .unwrap();
    let view = capture(connection, control);
    let held = Held {
        version: view.origin(1, control).unwrap().unwrap(),
        view,
        generation: first_stage.generation(),
    };
    canonical(connection, TABLE, FIELD, 2)
        .replace(1, &[vec![0.0, 1.0]], control)
        .unwrap();
    let (second, second_stage) = build(connection, control);
    connection
        .publish_diskann_generation(&second, &Resolver, control)
        .unwrap();
    held.check(control);
    let tiny = StorageReadControl::with_limit(1);
    assert!(held.view.selected_source(&Resolver, &tiny).is_err());
    assert_eq!(tiny.memory().used(), 0);
    let original = StorageReadControl::with_limit(1 << 20);
    let cancelled = capture(connection, &original)
        .selected_source(&Resolver, control)
        .unwrap()
        .unwrap();
    original.cancellation().cancel();
    assert!(cancelled
        .read_graph_pages(&[0], control, &mut |_, _| Ok(()))
        .is_err());
    (held, second_stage.generation())
}
