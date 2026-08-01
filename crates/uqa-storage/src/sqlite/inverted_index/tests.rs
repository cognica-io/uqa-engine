//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::sqlite::catalog::Catalog;
use uqa_analysis::{standard_analyzer, Analyzer, Tokenizer};

fn fields<const N: usize>(pairs: [(&str, &str); N]) -> BTreeMap<FieldName, String> {
    pairs
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn idx() -> SQLiteInvertedIndex {
    let mc = ManagedConnection::open_in_memory().unwrap();
    let _cat = Catalog::open(mc.clone()).unwrap();
    SQLiteInvertedIndex::new(mc, "articles", standard_analyzer("english"))
}

#[test]
fn add_get_round_trip() {
    let mut idx = idx();
    idx.add_document(1, fields([("title", "rust language")]))
        .unwrap();
    idx.add_document(2, fields([("title", "python language")]))
        .unwrap();
    let pl = idx.get_posting_list("title", "languag").unwrap();
    let docs: Vec<_> = pl.doc_ids().collect();
    assert_eq!(docs, vec![1, 2]);
}

#[test]
fn doc_freq_and_term_freq() {
    let mut idx = idx();
    idx.add_document(1, fields([("title", "rust rust rust")]))
        .unwrap();
    idx.add_document(2, fields([("title", "rust")])).unwrap();
    assert_eq!(idx.doc_freq("title", "rust").unwrap(), 2);
    assert_eq!(idx.get_term_freq(1, "title", "rust").unwrap(), 3);
    assert_eq!(idx.get_term_freq(2, "title", "rust").unwrap(), 1);
    let mut visited = Vec::new();
    idx.for_each_term_freq("title", "rust", &mut |doc_id, term_freq| {
        visited.push((doc_id, term_freq));
    })
    .unwrap();
    assert_eq!(visited, vec![(1, 3), (2, 1)]);
}

#[test]
fn bulk_posting_lists_match_point_lookups() {
    let mut idx = idx();
    idx.add_document(1, fields([("title", "rust language")]))
        .unwrap();
    idx.add_document(2, fields([("title", "python language")]))
        .unwrap();
    idx.add_document(3, fields([("title", "rust search")]))
        .unwrap();

    let terms = vec![
        "rust".to_string(),
        "languag".to_string(),
        "missing".to_string(),
        "rust".to_string(),
    ];
    let bulk = idx.get_posting_lists_bulk("title", &terms).unwrap();
    assert_eq!(bulk.len(), terms.len());
    for (term, posting_list) in terms.iter().zip(&bulk) {
        assert_eq!(posting_list, &idx.get_posting_list("title", term).unwrap());
    }
}

#[test]
fn bulk_doc_lengths_match_point_lookups() {
    let mut idx = idx();
    idx.add_document(1, fields([("title", "rust language")]))
        .unwrap();
    idx.add_document(2, fields([("title", "python")])).unwrap();
    idx.add_document(3, fields([("title", "sqlite search engine")]))
        .unwrap();

    let bulk = idx.get_doc_lengths_bulk(&[3, 1, 99, 2], "title").unwrap();
    assert_eq!(bulk.get(&1), Some(&idx.get_doc_length(1, "title").unwrap()));
    assert_eq!(bulk.get(&2), Some(&idx.get_doc_length(2, "title").unwrap()));
    assert_eq!(bulk.get(&3), Some(&idx.get_doc_length(3, "title").unwrap()));
    assert_eq!(bulk.get(&99), None);
}

#[test]
fn bulk_scoring_inputs_match_point_lookups_in_requested_order() {
    let mut idx = idx();
    idx.add_document(1, fields([("title", "rust rust language")]))
        .unwrap();
    idx.add_document(2, fields([("title", "python language")]))
        .unwrap();
    idx.add_document(3, fields([("title", "rust search engine")]))
        .unwrap();

    let doc_ids = [3, 1, 99, 2, 1];
    let terms = ["rust", "languag", "missing"].map(str::to_string);
    let bulk = idx
        .get_scoring_inputs_bulk(&doc_ids, "title", &terms)
        .unwrap();
    let expected: Vec<_> = doc_ids
        .iter()
        .map(|doc_id| {
            (
                idx.get_doc_length(*doc_id, "title").unwrap(),
                terms
                    .iter()
                    .map(|term| idx.get_term_freq(*doc_id, "title", term).unwrap())
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    assert_eq!(bulk, expected);
}

#[test]
fn negative_persisted_document_ids_are_rejected_by_every_posting_reader() {
    let idx = idx();
    idx.conn
        .with(|connection| {
            connection.execute(
                "INSERT INTO _postings
                    (table_name, field, term, doc_id, positions)
                 VALUES ('articles', 'title', 'rust', -1, ?1)",
                [positions_to_blob(&[0]).unwrap()],
            )?;
            Ok(())
        })
        .unwrap();

    let point_error = idx.get_posting_list("title", "rust").unwrap_err();
    assert!(point_error.to_string().contains("negative document id -1"));

    let terms = vec!["rust".to_string()];
    let bulk_error = idx.get_posting_lists_bulk("title", &terms).unwrap_err();
    assert!(bulk_error.to_string().contains("negative document id -1"));

    let mut visited = Vec::new();
    let visit_error = idx
        .for_each_term_freq("title", "rust", &mut |doc_id, frequency| {
            visited.push((doc_id, frequency));
        })
        .unwrap_err();
    assert!(visit_error.to_string().contains("negative document id -1"));
    assert!(visited.is_empty());

    let scoring_error = idx
        .get_scoring_inputs_bulk(&[1], "title", &terms)
        .unwrap_err();
    assert!(scoring_error
        .to_string()
        .contains("negative document id -1"));
}

#[test]
fn negative_persisted_lengths_and_block_indexes_are_rejected() {
    let idx = idx();
    idx.ensure_aux_tables("title").unwrap();
    let block_table = idx.blockmax_table_name("title");
    idx.conn
        .with(|connection| {
            connection.execute(
                "INSERT INTO _doc_lengths (table_name, doc_id, field, length)
                 VALUES ('articles', 1, 'title', -2)",
                [],
            )?;
            connection.execute(
                "INSERT INTO _field_stats (table_name, field, total_length)
                 VALUES ('articles', 'title', -2)",
                [],
            )?;
            connection.execute(
                &format!(
                    "INSERT INTO {} (term, block_idx, max_score) VALUES ('rust', -1, 1.0)",
                    quote_ident(&block_table)
                ),
                [],
            )?;
            Ok(())
        })
        .unwrap();

    let length_error = idx.get_doc_length(1, "title").unwrap_err();
    assert!(length_error
        .to_string()
        .contains("negative document length -2"));
    let bulk_error = idx.get_doc_lengths_bulk(&[1], "title").unwrap_err();
    assert!(bulk_error
        .to_string()
        .contains("negative document length -2"));
    let total_error = idx.total_field_length("title").unwrap_err();
    assert!(total_error
        .to_string()
        .contains("negative total field length -2"));
    let block_error = idx.get_all_block_max_scores("title", "rust").unwrap_err();
    assert!(block_error.to_string().contains("invalid block index -1"));
}

#[test]
fn document_ids_beyond_sqlite_integer_range_are_rejected_before_io() {
    let mut idx = idx();
    let add_error = idx
        .add_document(u64::MAX, fields([("title", "rust")]))
        .unwrap_err();
    assert!(add_error
        .to_string()
        .contains("exceeds the SQLite INTEGER range"));
    assert_eq!(idx.doc_count().unwrap(), 0);

    let lookup_error = idx.get_doc_length(u64::MAX, "title").unwrap_err();
    assert!(lookup_error
        .to_string()
        .contains("exceeds the SQLite INTEGER range"));
}

#[test]
fn position_count_matches_zero_based_u32_format() {
    validate_position_count(u64::from(u32::MAX) + 1).unwrap();
    let error = validate_position_count(u64::from(u32::MAX) + 2).unwrap_err();
    assert!(error.to_string().contains("u32 index format"));
}

#[test]
fn corrupt_total_rejects_remove_without_partial_delete() {
    let mut idx = idx();
    idx.add_document(1, fields([("title", "rust")])).unwrap();
    idx.conn
        .with(|connection| {
            connection.execute(
                "UPDATE _field_stats SET total_length = 0
                 WHERE table_name = 'articles' AND field = 'title'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

    let error = idx.remove_document(1).unwrap_err();
    assert!(error.to_string().contains("underflow"));
    assert_eq!(idx.doc_count().unwrap(), 1);
    assert_eq!(idx.get_doc_length(1, "title").unwrap(), 1);
    assert_eq!(idx.doc_freq("title", "rust").unwrap(), 1);
}

#[test]
fn sqlite_integer_counter_overflow_preserves_existing_index() {
    let mut idx = idx();
    idx.add_document(1, fields([("title", "rust")])).unwrap();
    idx.conn
        .with(|connection| {
            connection.execute(
                "UPDATE _field_stats SET total_length = ?1
                 WHERE table_name = 'articles' AND field = 'title'",
                [i64::MAX],
            )?;
            Ok(())
        })
        .unwrap();

    let error = idx
        .add_document(2, fields([("title", "sqlite")]))
        .unwrap_err();
    assert!(error.to_string().contains("SQLite INTEGER range"));
    assert_eq!(idx.doc_count().unwrap(), 1);
    assert_eq!(idx.doc_freq("title", "rust").unwrap(), 1);
    assert_eq!(idx.doc_freq("title", "sqlite").unwrap(), 0);
}

#[test]
fn rebuild_analysis_failure_preserves_existing_index() {
    let mut idx = idx();
    idx.add_document(1, fields([("title", "rust")])).unwrap();
    idx.set_field_analyzer(
        "body",
        Analyzer::new(
            Tokenizer::NGram {
                min_gram: 0,
                max_gram: 1,
            },
            Vec::new(),
            Vec::new(),
        ),
        AnalyzerPhase::Index,
    )
    .unwrap();

    let error = idx
        .try_rebuild_documents(vec![
            (2, fields([("title", "sqlite")])),
            (3, fields([("body", "failure")])),
        ])
        .unwrap_err();
    assert!(error.to_string().contains("gram"));
    assert_eq!(idx.doc_count().unwrap(), 1);
    assert_eq!(idx.doc_freq("title", "rust").unwrap(), 1);
    assert_eq!(idx.doc_freq("title", "sqlite").unwrap(), 0);
}

#[test]
fn rebuild_duplicate_document_uses_only_final_lengths() {
    let mut idx = idx();
    idx.try_rebuild_documents(vec![
        (1, fields([("title", "old old")])),
        (1, fields([("title", "new")])),
    ])
    .unwrap();

    assert_eq!(idx.doc_count().unwrap(), 1);
    assert_eq!(idx.total_field_length("title").unwrap(), 1);
    assert_eq!(idx.doc_freq("title", "old").unwrap(), 0);
    assert_eq!(idx.doc_freq("title", "new").unwrap(), 1);
}

#[test]
fn rebuild_documents_replaces_postings_and_stats() {
    let mut idx = idx();
    idx.add_document(1, fields([("title", "old rust")]))
        .unwrap();

    idx.try_rebuild_documents(vec![
        (2, fields([("title", "new search")])),
        (3, fields([("title", "new rust search")])),
    ])
    .unwrap();

    assert!(idx.get_posting_list("title", "old").unwrap().is_empty());
    assert_eq!(
        idx.get_posting_list("title", "new")
            .unwrap()
            .doc_ids()
            .collect::<Vec<_>>(),
        vec![2, 3]
    );
    assert_eq!(idx.doc_length_count(Some("title")).unwrap(), 2);
    assert_eq!(idx.total_field_length("title").unwrap(), 5);
}

#[test]
fn stats_match_memory_backend() {
    let mut idx = idx();
    idx.add_document(1, fields([("title", "rust language")]))
        .unwrap();
    idx.add_document(2, fields([("title", "rust")])).unwrap();
    let s = idx.stats().unwrap();
    assert_eq!(s.total_docs, 2);
    // After standard analyzer "rust language" -> ["rust", "languag"] (2)
    // and "rust" -> ["rust"] (1). avg = 3/2 = 1.5.
    assert!((s.avg_doc_length - 1.5).abs() < 1e-9);
    assert_eq!(s.doc_freq("title", "rust"), 2);
}

#[test]
fn replacing_doc_replaces_postings() {
    let mut idx = idx();
    idx.add_document(1, fields([("title", "rust")])).unwrap();
    idx.add_document(1, fields([("title", "go")])).unwrap();
    assert_eq!(idx.doc_freq("title", "rust").unwrap(), 0);
    assert_eq!(idx.doc_freq("title", "go").unwrap(), 1);
    assert_eq!(idx.doc_count().unwrap(), 1);
}

#[test]
fn remove_document_zeros_state() {
    let mut idx = idx();
    idx.add_document(1, fields([("title", "rust")])).unwrap();
    idx.add_document(2, fields([("title", "rust")])).unwrap();
    idx.remove_document(1).unwrap();
    assert_eq!(idx.doc_freq("title", "rust").unwrap(), 1);
    assert_eq!(idx.doc_count().unwrap(), 1);
    assert_eq!(idx.total_field_length("title").unwrap(), 1);
}
