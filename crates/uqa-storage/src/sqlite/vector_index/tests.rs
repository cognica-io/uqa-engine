use super::*;
use crate::sqlite::catalog::Catalog;

fn idx() -> SQLiteVectorIndex {
    let mc = ManagedConnection::open_in_memory().unwrap();
    let _cat = Catalog::open(mc.clone()).unwrap();
    SQLiteVectorIndex::new(mc, "articles", "embedding", 3)
}

#[test]
fn add_search_round_trip() {
    let mut idx = idx();
    idx.add(1, vec![1.0, 0.0, 0.0]).unwrap();
    idx.add(2, vec![0.0, 1.0, 0.0]).unwrap();
    idx.add(3, vec![0.7, 0.7, 0.0]).unwrap();
    let pl = idx.search_knn(&[1.0, 0.0, 0.0], 2).unwrap();
    let docs: Vec<_> = pl.doc_ids().collect();
    assert_eq!(docs, vec![1, 3]);
}

#[test]
fn delete_removes_vector() {
    let mut idx = idx();
    idx.add(1, vec![1.0, 0.0, 0.0]).unwrap();
    idx.delete(1).unwrap();
    assert_eq!(idx.count().unwrap(), 0);
}

#[test]
fn out_of_range_document_id_is_rejected_without_replacing_existing_vectors() {
    let mut idx = idx();
    idx.add(1, vec![1.0, 0.0, 0.0]).unwrap();

    let error = idx.add(u64::MAX, vec![0.0, 1.0, 0.0]).unwrap_err();
    assert!(error.to_string().contains("does not fit in SQLite INTEGER"));
    assert_eq!(idx.count().unwrap(), 1);
    assert_eq!(
        idx.search_knn(&[1.0, 0.0, 0.0], 1)
            .unwrap()
            .doc_ids()
            .collect::<Vec<_>>(),
        vec![1]
    );
}

#[test]
fn negative_persisted_document_id_is_reported_as_corruption() {
    let idx = idx();
    idx.conn
        .with(|conn| {
            conn.execute(
                "INSERT INTO _vectors
                   (table_name, field, doc_id, vector_ordinal, vector)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    "articles",
                    "embedding",
                    -1_i64,
                    0_i64,
                    vector_to_blob(&[1.0, 0.0, 0.0]).unwrap()
                ],
            )?;
            Ok(())
        })
        .unwrap();

    let error = idx.search_knn(&[1.0, 0.0, 0.0], 1).unwrap_err();
    assert!(error
        .to_string()
        .contains("invalid negative document id -1"));
}

#[test]
fn non_finite_vectors_queries_and_thresholds_are_rejected() {
    let mut idx = idx();
    assert!(idx.add(1, vec![f32::NAN, 0.0, 0.0]).is_err());
    idx.add(1, vec![1.0, 0.0, 0.0]).unwrap();
    assert!(idx.search_knn(&[f32::INFINITY, 0.0, 0.0], 1).is_err());
    assert!(idx.search_threshold(&[1.0, 0.0, 0.0], f32::NAN).is_err());
}

#[test]
fn round_trip_blob_preserves_bits() {
    let v = vec![0.1f32, -3.5, 12345.678];
    assert_eq!(blob_to_vector(&vector_to_blob(&v).unwrap()).unwrap(), v);
}

#[test]
fn vector_ordinal_count_matches_zero_based_u32_format() {
    validate_vector_ordinal_count(u64::from(u32::MAX) + 1).unwrap();
    let error = validate_vector_ordinal_count(u64::from(u32::MAX) + 2).unwrap_err();
    assert!(error.to_string().contains("u32 index format"));
}

#[test]
fn persisted_vector_ordinal_gaps_are_rejected() {
    let idx = idx();
    idx.conn
        .with(|connection| {
            connection.execute(
                "INSERT INTO _vectors
                   (table_name, field, doc_id, vector_ordinal, vector)
                 VALUES ('articles', 'embedding', 1, 1, ?1)",
                [vector_to_blob(&[1.0, 0.0, 0.0])?],
            )?;
            Ok(())
        })
        .unwrap();

    let error = idx.search_knn(&[1.0, 0.0, 0.0], 1).unwrap_err();
    assert!(error.to_string().contains("expected 0, found 1"));
}

#[test]
fn ivf_metadata_conversion_failure_does_not_insert_vectors() {
    let mc = ManagedConnection::open_in_memory().unwrap();
    let _catalog = Catalog::open(mc.clone()).unwrap();
    let mut idx = SQLiteIVFIndex::with_params(mc, "articles", "embedding", 2, usize::MAX, 1, 100);

    let error = idx.add(1, vec![1.0, 0.0]).unwrap_err();
    assert!(error.to_string().contains("nlist"));
    assert_eq!(idx.count().unwrap(), 0);
}

#[test]
fn ivf_metadata_write_failure_rolls_back_vector_replacement() {
    let mc = ManagedConnection::open_in_memory().unwrap();
    let _catalog = Catalog::open(mc.clone()).unwrap();
    let mut idx = SQLiteIVFIndex::with_params(mc.clone(), "articles", "embedding", 2, 4, 2, 100);
    idx.add(1, vec![1.0, 0.0]).unwrap();
    mc.with(|connection| {
        connection.execute_batch(
            "CREATE TRIGGER fail_ivf_metadata
             BEFORE INSERT ON _ivf_indexes
             BEGIN
                 SELECT RAISE(ABORT, 'injected IVF metadata failure');
             END;",
        )?;
        Ok(())
    })
    .unwrap();

    let error = idx.add(2, vec![0.0, 1.0]).unwrap_err();
    assert!(error.to_string().contains("injected IVF metadata failure"));
    assert_eq!(idx.count().unwrap(), 1);
    assert_eq!(
        idx.persistent
            .load_all_with_ordinals()
            .unwrap()
            .into_iter()
            .map(|(doc_id, _, _)| doc_id)
            .collect::<Vec<_>>(),
        vec![1]
    );
}

#[test]
fn sqlite_ivf_persists_metadata_and_reopens() {
    let mc = ManagedConnection::open_in_memory().unwrap();
    let _cat = Catalog::open(mc.clone()).unwrap();
    {
        let mut idx = SQLiteIVFIndex::with_params(mc.clone(), "articles", "embedding", 3, 3, 2, 3);
        idx.add(1, vec![1.0, 0.0, 0.0]).unwrap();
        idx.add(2, vec![0.0, 1.0, 0.0]).unwrap();
        idx.add(3, vec![0.8, 0.2, 0.0]).unwrap();
    }

    let centroid_count: i64 = mc
        .with(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM _ivf_centroids
                  WHERE table_name = 'articles' AND field = 'embedding'",
                [],
                |r| r.get(0),
            )
            .map_err(Into::into)
        })
        .unwrap();
    assert!(centroid_count > 0);

    let idx = SQLiteIVFIndex::with_params(mc, "articles", "embedding", 3, 3, 2, 3);
    assert_eq!(idx.index_kind(), "sqlite-ivf");
    assert_eq!(idx.count().unwrap(), 3);
    let pl = idx.search_knn(&[1.0, 0.0, 0.0], 2).unwrap();
    let docs: Vec<_> = pl.doc_ids().collect();
    assert_eq!(docs, vec![1, 3]);
}

#[test]
fn sqlite_ivf_uses_persisted_assignments_without_retraining() {
    let mc = ManagedConnection::open_in_memory().unwrap();
    let _cat = Catalog::open(mc.clone()).unwrap();
    let mut raw = SQLiteVectorIndex::new(mc.clone(), "articles", "embedding", 2);
    raw.add(1, vec![1.0, 0.0]).unwrap();
    raw.add(2, vec![0.0, 1.0]).unwrap();
    mc.with(|conn| {
        conn.execute(
            "INSERT INTO _ivf_indexes
                (table_name, field, dimensions, nlist, nprobe, train_threshold,
                 state, trained_size, deletes_since_train, vector_count)
             VALUES ('articles', 'embedding', 2, 2, 1, 2, 'trained', 2, 0, 2)",
            [],
        )?;
        conn.execute(
            "INSERT INTO _ivf_centroids (table_name, field, centroid_id, vector)
             VALUES ('articles', 'embedding', 0, ?1)",
            params![vector_to_blob(&[1.0, 0.0]).unwrap()],
        )?;
        conn.execute(
            "INSERT INTO _ivf_centroids (table_name, field, centroid_id, vector)
             VALUES ('articles', 'embedding', 1, ?1)",
            params![vector_to_blob(&[0.0, 1.0]).unwrap()],
        )?;
        // Deliberately inverted assignments. A rebuild from raw
        // vectors would put doc 1 in centroid 0; metadata reuse keeps
        // doc 2 as the only candidate for a [1, 0] query with nprobe=1.
        conn.execute(
            "INSERT INTO _ivf_assignments (table_name, field, doc_id, vector_ordinal, centroid_id)
             VALUES ('articles', 'embedding', 1, 0, 1)",
            [],
        )?;
        conn.execute(
            "INSERT INTO _ivf_assignments (table_name, field, doc_id, vector_ordinal, centroid_id)
             VALUES ('articles', 'embedding', 2, 0, 0)",
            [],
        )?;
        Ok(())
    })
    .unwrap();

    let idx = SQLiteIVFIndex::with_params(mc, "articles", "embedding", 2, 2, 1, 2);
    let pl = idx.search_knn(&[1.0, 0.0], 1).unwrap();
    let docs: Vec<_> = pl.doc_ids().collect();
    assert_eq!(docs, vec![2]);
}
