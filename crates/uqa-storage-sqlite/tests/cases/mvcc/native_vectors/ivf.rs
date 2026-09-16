//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native IVF mutations preserve the common owner's training state and assignments.

use super::*;
use uqa_storage::ivf_index::{IVFIndex, IVFMetadataSnapshot, IVFState};

fn matches_metadata(connection: &ManagedConnection, expected: &IVFMetadataSnapshot) {
    connection.with_physical(|sql| {
        let actual = sql.query_row("SELECT trained_size, deletes_since_train, vector_count FROM _ivf_indexes WHERE table_name = 'docs' AND field = 'embedding'", [], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?))
        })?;
        assert_eq!(actual, (i64::try_from(expected.trained_size).unwrap(), i64::try_from(expected.deletes_since_train).unwrap(), i64::try_from(expected.vector_count).unwrap()));
        let mut statement = sql.prepare("SELECT vector FROM _ivf_centroids WHERE table_name = 'docs' AND field = 'embedding' ORDER BY centroid_id")?;
        let centroids = statement.query_map([], |row| row.get::<_, Vec<u8>>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let expected_centroids = expected.centroids.iter().map(|vector| vector.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<_>>()).collect::<Vec<_>>();
        assert_eq!(centroids, expected_centroids);
        let mut statement = sql.prepare("SELECT doc_id, vector_ordinal, centroid_id FROM _ivf_assignments WHERE table_name = 'docs' AND field = 'embedding' ORDER BY doc_id, vector_ordinal")?;
        let assignments = statement.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, u32>(1)?, row.get::<_, i64>(2)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        assert_eq!(assignments, expected.assignments.iter().map(|(doc, ordinal, centroid)| (i64::try_from(*doc).unwrap(), *ordinal, i64::try_from(*centroid).unwrap())).collect::<Vec<_>>());
        Ok(())
    }).unwrap();
}

#[test]
fn native_ivf_mutations_preserve_centroids_and_retrain_at_the_common_delete_threshold() {
    for mode in MODES {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("ivf-training.db");
        let connection = open(mode, &path);
        Catalog::open(connection.clone()).unwrap();
        bind(&connection);
        let mut stored = index(&connection, IndexKind::Ivf, "docs");
        let mut reference = IVFIndex::with_params(3, 2, 2, 2);
        for doc in 1..=10 {
            let vector = vec![doc as f32, 11.0 - doc as f32, 0.5];
            stored.add(doc, vector.clone()).unwrap();
            reference.add(doc, vector).unwrap();
        }
        stored.initialize().unwrap();
        reference.initialize().unwrap();
        matches_metadata(&connection, &reference.metadata_snapshot());
        for doc in [1, 11] {
            stored.add(doc, Z.to_vec()).unwrap();
            reference.add(doc, Z.to_vec()).unwrap();
            matches_metadata(&connection, &reference.metadata_snapshot());
        }
        for doc in [2, 3, 4] {
            stored.delete(doc).unwrap();
            reference.delete(doc).unwrap();
            if reference.state() == IVFState::Stale {
                reference.train().unwrap();
            }
            matches_metadata(&connection, &reference.metadata_snapshot());
        }
        drop((stored, connection));
        let reopened = open(mode, &path);
        bind(&reopened);
        matches_metadata(&reopened, &reference.metadata_snapshot());
    }
}

#[test]
fn inconsistent_native_ivf_generations_reject_search_and_mutation_until_explicit_rebuild() {
    for mode in MODES {
        for corrupt in [
            "DELETE FROM _ivf_indexes",
            "UPDATE _ivf_indexes SET vector_count = vector_count + 1",
            "UPDATE _ivf_indexes SET nprobe = nprobe + 1",
            "DELETE FROM _ivf_centroids",
            "DELETE FROM _ivf_assignments",
            "UPDATE _ivf_assignments SET centroid_id = 9",
            "UPDATE _ivf_assignments SET doc_id = doc_id + 10",
        ] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("ivf-corruption.db");
            let connection = open(mode, &path);
            Catalog::open(connection.clone()).unwrap();
            let mut stored = index(&connection, IndexKind::Ivf, "docs");
            stored.add(1, X.to_vec()).unwrap();
            stored.add(2, Y.to_vec()).unwrap();
            stored.initialize().unwrap();
            connection
                .with_physical(|sql| {
                    sql.execute_batch(corrupt)?;
                    Ok(())
                })
                .unwrap();
            bind(&connection);
            assert!(stored.search_knn(&X, 2).is_err(), "{corrupt}");
            assert!(stored.add(3, Z.to_vec()).is_err(), "{corrupt}");
            assert_eq!(stored.count().unwrap(), 2);
            stored.initialize().unwrap();
            assert_eq!(stored.search_knn(&X, 2).unwrap().len(), 2);
            stored.add(3, Z.to_vec()).unwrap();
            assert_eq!(stored.count().unwrap(), 3);
        }
    }
}
