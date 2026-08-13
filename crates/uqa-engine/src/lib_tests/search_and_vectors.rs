//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn blob_to_vector(blob: &[u8]) -> Vec<f32> {
    blob.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

fn normalise(v: &mut [f32]) {
    let mag: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if mag > 1e-12 {
        for x in v {
            *x /= mag;
        }
    }
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

fn nearest_and_other_ivf_centroid(conn: &ManagedConnection, query: &[f32]) -> (i64, i64) {
    let mut query = query.to_vec();
    normalise(&mut query);
    conn.with(|conn| {
        let mut stmt = conn.prepare(
            "SELECT centroid_id, vector FROM _ivf_centroids
              WHERE table_name = 'public.articles' AND field = 'embedding'
              ORDER BY centroid_id",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)))?;
        let mut centroids = Vec::new();
        for row in rows {
            let (id, blob) = row?;
            let mut centroid = blob_to_vector(&blob);
            normalise(&mut centroid);
            centroids.push((id, centroid));
        }
        assert!(centroids.len() >= 2);
        let nearest = centroids
            .iter()
            .max_by(|(_, a), (_, b)| {
                dot(&query, a)
                    .partial_cmp(&dot(&query, b))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(id, _)| *id)
            .unwrap();
        let other = centroids
            .iter()
            .map(|(id, _)| *id)
            .find(|id| *id != nearest)
            .unwrap();
        Ok((nearest, other))
    })
    .unwrap()
}

fn make_doc_two_the_only_nearest_ivf_candidate(conn: &ManagedConnection, query: &[f32]) {
    let (nearest, other) = nearest_and_other_ivf_centroid(conn, query);
    conn.with(|conn| {
        conn.execute(
            "DELETE FROM _ivf_assignments
              WHERE table_name = 'public.articles' AND field = 'embedding'",
            [],
        )?;
        conn.execute(
            &format!(
                "INSERT INTO _ivf_assignments
                    (table_name, field, doc_id, centroid_id)
                 VALUES ('public.articles', 'embedding', 1, {other})"
            ),
            [],
        )?;
        conn.execute(
            &format!(
                "INSERT INTO _ivf_assignments
                    (table_name, field, doc_id, centroid_id)
                 VALUES ('public.articles', 'embedding', 2, {nearest})"
            ),
            [],
        )?;
        Ok(())
    })
    .unwrap();
}

fn stored_vector(conn: &ManagedConnection, doc_id: DocId) -> Vec<f32> {
    conn.with(|conn| {
        let blob: Vec<u8> = conn.query_row(
            "SELECT vector FROM _vectors
              WHERE table_name = 'public.articles'
                AND field = 'embedding'
                AND doc_id = ?1
              ORDER BY vector_ordinal
              LIMIT 1",
            [doc_id as i64],
            |r| r.get(0),
        )?;
        Ok(blob_to_vector(&blob))
    })
    .unwrap()
}

#[test]
fn run_analyze_populates_column_stats() {
    let eng = Engine::new();
    eng.create_default_table("docs", vec!["title".into()])
        .unwrap();
    // Register the columns directly through the table state so we
    // don't depend on the SQL DDL path here.
    if let Some(t) = eng.table("docs").unwrap() {
        *t.columns.write() = vec![uqa_sql::ast::ColumnDef {
            name: "title".into(),
            ty: uqa_sql::ast::ColumnType::Text,
            primary_key: false,
            not_null: false,
            not_null_explicit: false,
            not_null_name: None,
            auto_increment: false,
            unique: false,
            default: None,
            generated: None,
            check: None,
            check_name: None,
            check_enforced: true,
            references: None,
        }];
    }
    eng.add_document("docs", 1, doc([("title", s("alpha"))]))
        .unwrap();
    eng.add_document("docs", 2, doc([("title", s("alpha"))]))
        .unwrap();
    eng.add_document("docs", 3, doc([("title", s("beta"))]))
        .unwrap();
    eng.run_analyze(Some("docs")).unwrap();
    let stats = eng.column_stats("docs").unwrap();
    let title_stats = stats.get("title").expect("title stats");
    assert_eq!(title_stats.row_count, 3);
    assert_eq!(title_stats.distinct_count, 2);
    assert_eq!(title_stats.null_count, 0);
    // "alpha" appears twice and dominates the MCV list.
    assert_eq!(title_stats.mcv_values.first(), Some(&s("alpha")));
}

#[test]
fn add_get_delete_round_trip() {
    let eng = Engine::new();
    eng.create_default_table("articles", vec!["title".into()])
        .unwrap();
    eng.add_document("articles", 1, doc([("title", s("rust language"))]))
        .unwrap();
    let got = eng.get_document("articles", 1).unwrap().unwrap();
    assert_eq!(got.get("title"), Some(&s("rust language")));
    eng.delete_document("articles", 1).unwrap();
    assert!(eng.get_document("articles", 1).unwrap().is_none());
}

#[test]
fn search_returns_top_k_bm25_in_score_order() {
    let eng = Engine::new();
    eng.create_default_table("articles", vec!["title".into()])
        .unwrap();
    eng.add_document(
        "articles",
        1,
        doc([("title", s("the rust programming language"))]),
    )
    .unwrap();
    eng.add_document("articles", 2, doc([("title", s("python language guide"))]))
        .unwrap();
    eng.add_document("articles", 3, doc([("title", s("rust rust rust"))]))
        .unwrap();

    let hits = eng
        .search(
            "articles",
            "title",
            "rust",
            &ScoringMode::BM25(BM25Params::default()),
            10,
        )
        .unwrap();
    // Doc 3 has tf=3 and is shorter -> highest BM25.
    assert_eq!(hits.first().map(|h| h.doc_id), Some(3));
    assert!(hits.iter().any(|h| h.doc_id == 1));
    assert!(hits.iter().all(|h| h.doc_id != 2));
}

#[test]
fn search_top_k_matches_full_score_prefix() {
    let eng = Engine::new();
    eng.create_default_table("articles", vec!["title".into()])
        .unwrap();
    for doc_id in 1..=20 {
        let body = std::iter::repeat_n("rust", doc_id as usize)
            .collect::<Vec<_>>()
            .join(" ");
        eng.add_document("articles", doc_id, doc([("title", s(&body))]))
            .unwrap();
    }

    let full = eng
        .search(
            "articles",
            "title",
            "rust",
            &ScoringMode::BM25(BM25Params::default()),
            usize::MAX,
        )
        .unwrap();
    let top = eng
        .search(
            "articles",
            "title",
            "rust",
            &ScoringMode::BM25(BM25Params::default()),
            3,
        )
        .unwrap();

    assert_eq!(top.len(), 3);
    assert_eq!(
        top.iter().map(|hit| hit.doc_id).collect::<Vec<_>>(),
        full.iter()
            .take(3)
            .map(|hit| hit.doc_id)
            .collect::<Vec<_>>()
    );
}

#[test]
fn search_returns_calibrated_probabilities_under_bayesian_bm25() {
    let eng = Engine::new();
    eng.create_default_table("articles", vec!["title".into()])
        .unwrap();
    eng.add_document(
        "articles",
        1,
        doc([("title", s("the rust programming language"))]),
    )
    .unwrap();
    eng.add_document("articles", 2, doc([("title", s("python is dynamic"))]))
        .unwrap();

    let hits = eng
        .search(
            "articles",
            "title",
            "rust",
            &ScoringMode::BayesianBM25(BayesianBM25Params::default()),
            10,
        )
        .unwrap();

    // Bayesian BM25 always returns probabilities in (0, 1).
    for h in &hits {
        assert!(
            (0.0..=1.0).contains(&h.score),
            "score {} out of [0, 1]",
            h.score
        );
    }
    assert_eq!(hits.first().map(|h| h.doc_id), Some(1));
}

#[test]
fn knn_returns_top_k_in_descending_similarity() {
    let eng = Engine::new();
    eng.create_default_table("articles", vec!["title".into()])
        .unwrap();
    eng.create_vector_field("articles", "embedding", 3).unwrap();
    eng.add_document_with_vectors(
        "articles",
        1,
        doc([("title", s("a"))]),
        BTreeMap::from([("embedding".into(), vec![1.0, 0.0, 0.0])]),
    )
    .unwrap();
    eng.add_document_with_vectors(
        "articles",
        2,
        doc([("title", s("b"))]),
        BTreeMap::from([("embedding".into(), vec![0.0, 1.0, 0.0])]),
    )
    .unwrap();
    eng.add_document_with_vectors(
        "articles",
        3,
        doc([("title", s("c"))]),
        BTreeMap::from([("embedding".into(), vec![0.7, 0.7, 0.0])]),
    )
    .unwrap();

    let hits = eng
        .knn_search("articles", "embedding", vec![1.0, 0.0, 0.0], 2)
        .unwrap();
    assert_eq!(hits.first().map(|h| h.doc_id), Some(1));
    // doc 3 (cos ~0.707) beats doc 2 (cos 0.0).
    assert_eq!(hits.get(1).map(|h| h.doc_id), Some(3));
}

#[test]
fn vector_fields_use_bruteforce_until_explicit_ivf_index() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE articles (id INTEGER PRIMARY KEY, embedding VECTOR(3))",
        &[],
    )
    .unwrap();
    assert_eq!(
        vector_index_kind(&eng, "articles", "embedding"),
        "memory-bruteforce"
    );
    eng.sql(
        "CREATE INDEX articles_embedding_ivf ON articles USING ivf (embedding)",
        &[],
    )
    .unwrap();
    assert_eq!(vector_index_kind(&eng, "articles", "embedding"), "ivf");

    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("vectors.db");
    {
        let eng = Engine::open(&db).unwrap();
        eng.sql(
            "CREATE TABLE articles (id INTEGER PRIMARY KEY, embedding VECTOR(3))",
            &[],
        )
        .unwrap();
        assert_eq!(
            vector_index_kind(&eng, "articles", "embedding"),
            "sqlite-bruteforce"
        );
        eng.sql(
            "CREATE INDEX articles_embedding_ivf ON articles USING ivf (embedding)",
            &[],
        )
        .unwrap();
        assert_eq!(
            vector_index_kind(&eng, "articles", "embedding"),
            "sqlite-ivf"
        );
    }

    let eng = Engine::open(&db).unwrap();
    assert_eq!(
        vector_index_kind(&eng, "articles", "embedding"),
        "sqlite-ivf"
    );
}

#[test]
fn hnsw_is_a_distinct_persistent_index_and_survives_reopen() {
    let memory = Engine::new();
    memory
        .sql(
            "CREATE TABLE articles (id INTEGER PRIMARY KEY, embedding VECTOR(2))",
            &[],
        )
        .unwrap();
    memory
        .sql(
            "CREATE INDEX articles_embedding_hnsw ON articles USING hnsw (embedding) \
             WITH (m = 4, ef_construction = 24, ef_search = 16, seed = 7)",
            &[],
        )
        .unwrap();
    assert_eq!(vector_index_kind(&memory, "articles", "embedding"), "hnsw");

    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("hnsw.db");
    {
        let engine = Engine::open(&database).unwrap();
        engine
            .sql(
                "CREATE TABLE articles (id INTEGER PRIMARY KEY, embedding VECTOR(2))",
                &[],
            )
            .unwrap();
        engine
            .sql(
                "INSERT INTO articles (id, embedding) VALUES \
                 (1, ARRAY[1.0, 0.0]), (2, ARRAY[0.0, 1.0]), (3, ARRAY[-1.0, 0.0])",
                &[],
            )
            .unwrap();
        engine
            .sql(
                "CREATE INDEX articles_embedding_hnsw ON articles USING hnsw (embedding) \
                 WITH (m = 4, ef_construction = 24, ef_search = 16, seed = 7)",
                &[],
            )
            .unwrap();
        assert_eq!(vector_index_kind(&engine, "articles", "embedding"), "hnsw");
        assert_eq!(
            engine
                .knn_search("articles", "embedding", vec![-1.0, 0.0], 1)
                .unwrap()[0]
                .doc_id,
            3
        );
    }
    let connection = ManagedConnection::open(&database).unwrap();
    let (kind, nodes): (String, i64) = connection
        .with(|conn| {
            Ok((
                conn.query_row(
                    "SELECT index_type FROM _catalog_indexes
                      WHERE name = 'articles_embedding_hnsw'",
                    [],
                    |row| row.get(0),
                )?,
                conn.query_row(
                    "SELECT COUNT(*) FROM _hnsw_nodes
                      WHERE table_name = 'public.articles' AND field = 'embedding'",
                    [],
                    |row| row.get(0),
                )?,
            ))
        })
        .unwrap();
    assert_eq!(kind, "hnsw");
    assert_eq!(nodes, 3);

    let reopened = Engine::open(&database).unwrap();
    assert_eq!(
        vector_index_kind(&reopened, "articles", "embedding"),
        "hnsw"
    );
    assert_eq!(
        reopened
            .knn_search("articles", "embedding", vec![1.0, 0.0], 1)
            .unwrap()[0]
            .doc_id,
        1
    );
}

#[test]
fn sqlite_reopen_repairs_legacy_hnsw_alias_backed_by_ivf() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("legacy-hnsw-alias.db");
    {
        let engine = Engine::open(&database).unwrap();
        engine
            .sql(
                "CREATE TABLE articles (id INTEGER PRIMARY KEY, embedding VECTOR(2))",
                &[],
            )
            .unwrap();
        engine
            .sql(
                "INSERT INTO articles (id, embedding) VALUES
                 (1, ARRAY[1.0, 0.0]), (2, ARRAY[0.0, 1.0])",
                &[],
            )
            .unwrap();
        engine
            .sql(
                "CREATE INDEX articles_embedding_idx ON articles USING ivf (embedding)",
                &[],
            )
            .unwrap();
    }
    let connection = ManagedConnection::open(&database).unwrap();
    connection
        .with(|conn| {
            conn.execute(
                "UPDATE _catalog_indexes SET index_type = 'hnsw'
                  WHERE name = 'articles_embedding_idx'",
                [],
            )?;
            conn.execute(
                "UPDATE _metadata SET value = '19'
                  WHERE key = 'schema_version'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    drop(connection);

    let reopened = Engine::open(&database).unwrap();
    assert_eq!(
        vector_index_kind(&reopened, "articles", "embedding"),
        "sqlite-ivf"
    );
    assert_eq!(
        reopened
            .knn_search("articles", "embedding", vec![1.0, 0.0], 1)
            .unwrap()[0]
            .doc_id,
        1
    );
    assert_eq!(
        reopened
            .catalog_index("articles_embedding_idx")
            .unwrap()
            .unwrap()
            .index_type,
        "ivf"
    );
}

#[test]
fn sqlite_ivf_restore_reuses_persisted_assignments() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("vectors.db");
    {
        let eng = Engine::open(&db).unwrap();
        eng.sql(
            "CREATE TABLE articles (id INTEGER PRIMARY KEY, embedding VECTOR(2))",
            &[],
        )
        .unwrap();
        eng.sql(
            "INSERT INTO articles (id, embedding) VALUES \
             (1, ARRAY[1.0, 0.0]), \
             (2, ARRAY[0.0, 1.0])",
            &[],
        )
        .unwrap();
        eng.sql(
            "CREATE INDEX articles_embedding_ivf ON articles USING ivf (embedding) \
             WITH (lists = 2, probes = 1, train_threshold = 2)",
            &[],
        )
        .unwrap();

        let conn = ManagedConnection::open(&db).unwrap();
        make_doc_two_the_only_nearest_ivf_candidate(&conn, &[1.0, 0.0]);
    }

    let reopened = Engine::open(&db).unwrap();
    assert_eq!(
        vector_index_kind(&reopened, "articles", "embedding"),
        "sqlite-ivf"
    );
    let hits = reopened
        .knn_search("articles", "embedding", vec![1.0, 0.0], 1)
        .unwrap();
    assert_eq!(hits.first().map(|h| h.doc_id), Some(2));
}

#[test]
fn sqlite_ivf_create_index_reuses_existing_persistent_vectors() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("vectors.db");
    let eng = Engine::open(&db).unwrap();
    eng.sql(
        "CREATE TABLE articles (id INTEGER PRIMARY KEY, embedding VECTOR(2))",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO articles (id, embedding) VALUES \
         (1, ARRAY[1.0, 0.0]), \
         (2, ARRAY[0.0, 1.0])",
        &[],
    )
    .unwrap();

    let conn = ManagedConnection::open(&db).unwrap();
    conn.with(|conn| {
        conn.execute(
            "UPDATE _documents
                SET body = json_set(body, '$.embedding', json('[0.0, 1.0]'))
              WHERE table_name = 'public.articles' AND doc_id = 1",
            [],
        )?;
        conn.execute(
            "UPDATE _documents
                SET body = json_set(body, '$.embedding', json('[1.0, 0.0]'))
              WHERE table_name = 'public.articles' AND doc_id = 2",
            [],
        )?;
        Ok(())
    })
    .unwrap();

    eng.sql(
        "CREATE INDEX articles_embedding_ivf ON articles USING ivf (embedding) \
         WITH (lists = 2, probes = 1, train_threshold = 2)",
        &[],
    )
    .unwrap();

    assert_eq!(stored_vector(&conn, 1), vec![1.0, 0.0]);
    assert_eq!(stored_vector(&conn, 2), vec![0.0, 1.0]);
}

#[test]
fn create_vector_field_backfills_existing_documents() {
    let eng = Engine::new();
    eng.create_default_table("docs", vec![]).unwrap();
    eng.add_document("docs", 1, doc([("embedding", vector(&[1.0, 0.0]))]))
        .unwrap();
    eng.add_document("docs", 2, doc([("embedding", vector(&[0.0, 1.0]))]))
        .unwrap();
    eng.add_document("docs", 3, doc([("embedding", vector(&[0.8, 0.2]))]))
        .unwrap();

    assert!(eng.create_vector_field("docs", "embedding", 2).unwrap());
    let hits = eng
        .knn_search("docs", "embedding", vec![1.0, 0.0], 2)
        .unwrap();
    assert_eq!(
        hits.iter().map(|h| h.doc_id).collect::<Vec<_>>(),
        vec![1, 3]
    );
}

#[test]
fn hybrid_search_combines_text_and_vector_signals() {
    let eng = Engine::new();
    eng.create_default_table("articles", vec!["title".into()])
        .unwrap();
    eng.create_vector_field("articles", "embedding", 3).unwrap();

    // Doc 1: title matches "rust", embedding pointing toward query.
    eng.add_document_with_vectors(
        "articles",
        1,
        doc([("title", s("rust language"))]),
        BTreeMap::from([("embedding".into(), vec![1.0, 0.0, 0.0])]),
    )
    .unwrap();
    // Doc 2: title matches "rust", embedding orthogonal to query.
    eng.add_document_with_vectors(
        "articles",
        2,
        doc([("title", s("rust ecosystem"))]),
        BTreeMap::from([("embedding".into(), vec![0.0, 1.0, 0.0])]),
    )
    .unwrap();
    // Doc 3: no text match, embedding near query.
    eng.add_document_with_vectors(
        "articles",
        3,
        doc([("title", s("python programming"))]),
        BTreeMap::from([("embedding".into(), vec![0.95, 0.1, 0.0])]),
    )
    .unwrap();

    let hits = eng
        .hybrid_search(&HybridSearchParams {
            table: "articles",
            text_field: "title",
            text_query: "rust",
            vector_field: "embedding",
            query_vector: vec![1.0, 0.0, 0.0],
            knn_pool: 10,
            top_k: 10,
        })
        .unwrap();

    // Doc 1 should rank highest: text match AND high cosine.
    assert_eq!(hits.first().map(|h| h.doc_id), Some(1));
    // All three should appear (after coverage-based defaults fill
    // missing signals).
    let ids: Vec<DocId> = hits.iter().map(|h| h.doc_id).collect();
    assert!(ids.contains(&1) && ids.contains(&2) && ids.contains(&3));
}

#[test]
fn document_count_tracks_indexed_documents() {
    let eng = Engine::new();
    eng.create_default_table("articles", vec!["title".into()])
        .unwrap();
    for i in 0..5 {
        eng.add_document("articles", i, doc([("title", s(&format!("doc {i}")))]))
            .unwrap();
    }
    assert_eq!(eng.document_count("articles").unwrap(), 5);
}
