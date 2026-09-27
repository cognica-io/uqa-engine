//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::{read_control::StorageReadControl, StorageBackendError};

mod unvisited;

#[test]
fn diskann_sql_serializable_snapshot_preserves_invocation_controls() {
    for provider in 0..3 {
        let (_directory, engine, _peer) = sessions(provider);
        sql(&engine, "CREATE TABLE diskann_docs(id int, embedding tensor(2)); INSERT INTO diskann_docs VALUES(1,ARRAY[ARRAY[1.0,0.0]]); CREATE INDEX diskann_idx ON diskann_docs USING diskann(embedding)");
        sql(&engine, "BEGIN ISOLATION LEVEL SERIALIZABLE");
        engine.prepare_serializable_transaction_snapshot().unwrap();
        let table = engine.try_table("diskann_docs").unwrap().unwrap();
        let read = engine
            .serializable_table_state_read(&table)
            .unwrap()
            .unwrap();
        let snapshot = table
            .vector_indexes
            .read()
            .get("embedding")
            .unwrap()
            .snapshot()
            .unwrap();
        let observed = uqa_execution::serializable::vector::observe_snapshot(
            Some(&read),
            &table.columns.read(),
            "embedding",
            snapshot,
        )
        .unwrap();
        let empty = StorageReadControl::with_limit(0);
        assert_eq!(
            observed
                .search_knn_with_control(&[1.0, 0.0], 1, &empty)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            observed
                .search_threshold_with_control(&[1.0, 0.0], 0.5, &empty)
                .unwrap()
                .len(),
            1
        );
        single_vector_reports(&*observed, &empty);
        assert_eq!(empty.memory().used(), 0);
        assert!(observed
            .search_knn_with_control(&[1.0, 0.0], 0, &empty)
            .unwrap()
            .is_empty());
        let query = StorageReadControl::with_limit(1 << 20);
        let nested = observed.snapshot().unwrap();
        let owner = engine.query_retention_control().unwrap();
        let held = owner
            .memory()
            .reserve(owner.memory().limit() - owner.memory().used())
            .unwrap();
        assert!(matches!(
            nested.search_knn_with_control(&[1.0, 0.0], 1, &query),
            Err(StorageBackendError::Memory(_))
        ));
        assert!(matches!(
            nested.search_threshold_with_control(&[1.0, 0.0], 0.5, &query),
            Err(StorageBackendError::Memory(_))
        ));
        assert_eq!(query.memory().used(), 0);
        assert!(matches!(
            nested.search_knn_with_statistics(&[1.0, 0.0], 1, Some(&query)),
            Err(StorageBackendError::Memory(_))
        ));
        assert!(matches!(
            nested.search_threshold_with_statistics(&[1.0, 0.0], 0.5, Some(&query)),
            Err(StorageBackendError::Memory(_))
        ));
        drop(held);
        assert_eq!(
            nested
                .search_knn_with_control(&[1.0, 0.0], 1, &query)
                .unwrap()
                .len(),
            1
        );
        query.cancellation().cancel();
        assert!(matches!(
            nested.search_knn_with_control(&[1.0, 0.0], 1, &query),
            Err(StorageBackendError::Cancelled(_))
        ));
        assert!(matches!(
            nested.search_threshold_with_control(&[1.0, 0.0], 0.5, &query),
            Err(StorageBackendError::Cancelled(_))
        ));
        assert!(matches!(
            nested.search_knn_with_statistics(&[1.0, 0.0], 1, Some(&query)),
            Err(StorageBackendError::Cancelled(_))
        ));
        assert!(matches!(
            nested.search_threshold_with_statistics(&[1.0, 0.0], 0.5, Some(&query)),
            Err(StorageBackendError::Cancelled(_))
        ));
        sql(&engine, "ROLLBACK");
    }
}

#[test]
fn diskann_sql_serializable_reads_cover_nonreturned_candidates_but_not_zero_k_or_explain() {
    for provider in 0..3 {
        for mode in ["zero", "explain", "calibration_metadata", "search"] {
            let (_directory, first, second) = sessions(provider);
            sql(&first, "CREATE TABLE diskann_docs(id int PRIMARY KEY, embedding vector(2)); INSERT INTO diskann_docs VALUES(1,ARRAY[0.0,1.0]),(2,ARRAY[-1.0,0.0]); CREATE INDEX diskann_idx ON diskann_docs USING diskann(embedding)");
            sql(&first, "BEGIN ISOLATION LEVEL SERIALIZABLE");
            sql(&second, "BEGIN ISOLATION LEVEL SERIALIZABLE");
            if mode == "zero" {
                assert!(first
                    .knn_search("diskann_docs", "embedding", [1.0, 0.0], 0)
                    .unwrap()
                    .is_empty());
            } else if mode == "explain" {
                sql(&first, "EXPLAIN SELECT id FROM diskann_docs WHERE knn_match(embedding,ARRAY[1.0,0.0],1)");
            } else if mode == "calibration_metadata" {
                first
                    .diskann_calibration_target("diskann_docs", "embedding", "fixture", "1", 1)
                    .unwrap();
            } else {
                let found = sql(
                    &first,
                    "SELECT id FROM diskann_docs WHERE knn_match(embedding,ARRAY[1.0,0.0],1)",
                );
                assert_eq!(found.rows.len(), 1);
                assert_eq!(found.rows[0]["id"], Value::Int(1));
            }
            sql(&second, "SELECT v FROM t");
            sql(&first, "UPDATE t SET v=2");
            sql(
                &second,
                "UPDATE diskann_docs SET embedding=ARRAY[1.0,0.0] WHERE id=2",
            );
            let outcomes = [first.commit(), second.commit()];
            if mode == "search" {
                assert!(
                    outcomes.iter().any(Result::is_err),
                    "candidate dependency cycle committed"
                );
                for error in outcomes.into_iter().filter_map(Result::err) {
                    assert_eq!(error.sqlstate(), Some("40001"), "{error}");
                }
            } else {
                assert!(outcomes.iter().all(Result::is_ok), "{outcomes:?}");
            }
        }
    }
}

#[test]
fn diskann_selected_source_snapshots_keep_original_serializable_observation() {
    use uqa_storage::diskann_index::DiskANNReadChanges;
    for provider in 0..3 {
        for mode in [
            "metadata",
            "zero",
            "knn",
            "threshold",
            "report_zero",
            "report_knn",
            "report_threshold",
            "report_invalid",
        ] {
            let (_directory, first, second) = sessions(provider);
            sql(&first, "CREATE TABLE diskann_docs(id int PRIMARY KEY, embedding vector(2)); INSERT INTO diskann_docs VALUES(1,ARRAY[0.0,1.0]),(2,ARRAY[-1.0,0.0]); CREATE INDEX diskann_idx ON diskann_docs USING diskann(embedding)");
            sql(&first, "BEGIN ISOLATION LEVEL SERIALIZABLE");
            sql(&second, "BEGIN ISOLATION LEVEL SERIALIZABLE");
            first.prepare_serializable_transaction_snapshot().unwrap();
            let table = first.try_table("diskann_docs").unwrap().unwrap();
            let document = table.document_store.read().next_doc_ids(None, 1).unwrap()[0];
            let read = first
                .serializable_table_state_read(&table)
                .unwrap()
                .unwrap();
            let snapshot = table
                .vector_indexes
                .read()
                .get("embedding")
                .unwrap()
                .snapshot()
                .unwrap();
            let observed = uqa_execution::serializable::vector::observe_snapshot(
                Some(&read),
                &table.columns.read(),
                "embedding",
                snapshot,
            )
            .unwrap();
            let control = first.query_retention_control().unwrap();
            let source = observed.diskann_read_snapshot(&control).unwrap().unwrap();
            let changes =
                DiskANNReadChanges::capture([Ok((document, Some(source)))], &control).unwrap();
            let projected = observed
                .snapshot_with_diskann_changes(&changes, &control)
                .unwrap()
                .unwrap()
                .snapshot()
                .unwrap();
            assert_eq!(projected.index_kind(), "diskann");
            assert_eq!(projected.count().unwrap(), 2);
            assert!(projected.contains_document(document).unwrap());
            selected_search(&*projected, mode, &control);
            sql(&second, "SELECT v FROM t");
            sql(&first, "UPDATE t SET v=2");
            sql(
                &second,
                "UPDATE diskann_docs SET embedding=ARRAY[1.0,0.0] WHERE id=2",
            );
            let outcomes = [first.commit(), second.commit()];
            if matches!(
                mode,
                "knn" | "threshold" | "report_knn" | "report_threshold"
            ) {
                assert!(
                    outcomes.iter().any(Result::is_err),
                    "projected candidate cycle committed: {provider}/{mode}"
                );
                for error in outcomes.into_iter().filter_map(Result::err) {
                    assert_eq!(error.sqlstate(), Some("40001"), "{error}");
                }
            } else {
                assert!(
                    outcomes.iter().all(Result::is_ok),
                    "metadata observed a query: {provider}/{mode}: {outcomes:?}"
                );
            }
        }
    }
}

fn selected_search(
    projected: &dyn uqa_storage::VectorIndex,
    mode: &str,
    control: &StorageReadControl,
) {
    match mode {
        "metadata" => (),
        "zero" => assert!(projected.search_knn(&[1.0, 0.0], 0).unwrap().is_empty()),
        "knn" => assert_eq!(projected.search_knn(&[1.0, 0.0], 1).unwrap().len(), 1),
        "threshold" => assert_eq!(
            projected.search_threshold(&[1.0, 0.0], 0.0).unwrap().len(),
            1
        ),
        "report_zero" => {
            let result = projected
                .search_knn_with_statistics(&[1.0, 0.0], 0, Some(control))
                .unwrap();
            assert!(result.postings.is_empty());
            assert_eq!(
                result.diskann.unwrap().route,
                uqa_storage::vector_index::DiskANNExecutionRoute::EmptyK
            );
        }
        "report_knn" => {
            let result = projected
                .search_knn_with_statistics(&[1.0, 0.0], 1, Some(control))
                .unwrap();
            assert_eq!(result.postings.len(), 1);
            assert!(result.diskann.is_some());
        }
        "report_threshold" => {
            let result = projected
                .search_threshold_with_statistics(&[1.0, 0.0], 0.0, Some(control))
                .unwrap();
            assert_eq!(result.postings.len(), 1);
            assert_eq!(result.diskann.unwrap().work.exact.vectors, 2);
        }
        "report_invalid" => {
            assert!(projected
                .search_knn_with_statistics(&[1.0], 1, None)
                .is_err());
            assert!(projected
                .search_threshold_with_statistics(&[1.0, 0.0], f32::NAN, None)
                .is_err());
        }
        _ => unreachable!(),
    }
}

fn single_vector_reports(observed: &dyn uqa_storage::VectorIndex, empty: &StorageReadControl) {
    let reported = observed
        .search_knn_with_statistics(&[1.0, 0.0], 1, Some(empty))
        .unwrap();
    assert_eq!(reported.postings.len(), 1);
    assert!(reported.diskann.is_some());
    let reported = observed
        .search_threshold_with_statistics(&[1.0, 0.0], 0.5, Some(empty))
        .unwrap();
    assert_eq!(reported.diskann.unwrap().work.exact.vectors, 1);
}
