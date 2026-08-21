//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Skip-pointer and block-max index coverage.

use std::collections::BTreeMap;

use uqa_analysis::standard_analyzer;
use uqa_core::PostingList;
use uqa_storage::sqlite::{Catalog, ManagedConnection};
use uqa_storage::{BlockMaxIndex, BlockMaxScorer, InvertedIndex, SQLiteInvertedIndex};

fn sqlite_conn() -> ManagedConnection {
    let conn = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(conn.clone()).unwrap();
    conn
}

fn file_conn(path: &std::path::Path) -> ManagedConnection {
    let conn = ManagedConnection::open(path).unwrap();
    Catalog::open(conn.clone()).unwrap();
    conn
}

fn make_index(conn: ManagedConnection, table: &str) -> SQLiteInvertedIndex {
    SQLiteInvertedIndex::new(conn, table, standard_analyzer("english"))
}

fn fields(pairs: &[(&str, String)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(field, value)| ((*field).to_string(), value.clone()))
        .collect()
}

struct TestScorer;

impl BlockMaxScorer for TestScorer {
    fn score(&self, term_freq: u64, doc_length: u64, doc_freq: u64) -> f64 {
        let normalized_tf = term_freq as f64 / doc_length.max(1) as f64;
        normalized_tf + 1.0 / doc_freq.max(1) as f64
    }
}

fn scorer(_total_docs: u64, _avg_doc_length: f64) -> TestScorer {
    TestScorer
}

#[test]
fn test_skip_table_created() {
    let conn = sqlite_conn();
    let mut idx = make_index(conn.clone(), "docs");
    idx.add_document(1, fields(&[("body", "hello world".into())]))
        .unwrap();
    conn.with(|c| {
        let count: i64 = c.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name = '_skip_docs_body'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(count, 1);
        Ok(())
    })
    .unwrap();
}

#[test]
fn test_skip_entries_for_small_posting_list() {
    let conn = sqlite_conn();
    let mut idx = make_index(conn.clone(), "docs");
    for i in 1..=3 {
        idx.add_document(i, fields(&[("body", "hello".into())]))
            .unwrap();
    }
    idx.flush_skip_pointers().unwrap();
    assert_eq!(idx.skip_to("body", "hello", 10).unwrap(), (1, 0));
}

#[test]
fn test_skip_entries_for_large_posting_list() {
    let conn = sqlite_conn();
    let mut idx = make_index(conn.clone(), "docs");
    for i in 1..=300 {
        idx.add_document(i, fields(&[("body", "alpha".into())]))
            .unwrap();
    }
    idx.flush_skip_pointers().unwrap();
    conn.with(|c| {
        let mut stmt = c.prepare(
            "SELECT skip_doc_id, skip_offset FROM \"_skip_docs_body\"
             WHERE term = 'alpha' ORDER BY skip_offset",
        )?;
        let rows = stmt
            .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        assert_eq!(rows, vec![(1, 0), (129, 128), (257, 256)]);
        Ok(())
    })
    .unwrap();
}

#[test]
fn test_skip_rebuilt_on_remove() {
    let conn = sqlite_conn();
    let mut idx = make_index(conn, "docs");
    for i in 1..200 {
        idx.add_document(i, fields(&[("body", "alpha".into())]))
            .unwrap();
    }
    idx.flush_skip_pointers().unwrap();
    assert_eq!(idx.skip_to("body", "alpha", 150).unwrap(), (129, 128));
    for i in 1..73 {
        idx.remove_document(i).unwrap();
    }
    idx.flush_skip_pointers().unwrap();
    assert_eq!(idx.skip_to("body", "alpha", 150).unwrap(), (73, 0));
}

#[test]
fn test_skip_to_finds_nearest() {
    let conn = sqlite_conn();
    let mut idx = make_index(conn, "docs");
    for i in 1..=300 {
        idx.add_document(i, fields(&[("body", "alpha".into())]))
            .unwrap();
    }
    idx.flush_skip_pointers().unwrap();
    assert_eq!(idx.skip_to("body", "alpha", 150).unwrap(), (129, 128));
}

#[test]
fn test_skip_to_exact_match() {
    let conn = sqlite_conn();
    let mut idx = make_index(conn, "docs");
    for i in 1..=300 {
        idx.add_document(i, fields(&[("body", "alpha".into())]))
            .unwrap();
    }
    idx.flush_skip_pointers().unwrap();
    assert_eq!(idx.skip_to("body", "alpha", 129).unwrap(), (129, 128));
}

#[test]
fn skip_lookup_never_materializes_storage() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("read-only-skip.db");
    let conn = file_conn(&db);
    let observer = conn.new_session();
    let mut idx = make_index(conn, "docs");
    for i in 1..=300 {
        idx.add_document(i, fields(&[("body", "alpha".into())]))
            .unwrap();
    }

    let before = observer.data_version().unwrap();
    assert_eq!(idx.skip_to("body", "alpha", 150).unwrap(), (0, 0));
    assert_eq!(observer.data_version().unwrap(), before);

    idx.flush_skip_pointers().unwrap();
    assert_eq!(idx.skip_to("body", "alpha", 150).unwrap(), (129, 128));
}

#[test]
fn test_skip_to_before_first() {
    let conn = sqlite_conn();
    let mut idx = make_index(conn, "docs");
    for i in 100..200 {
        idx.add_document(i, fields(&[("body", "alpha".into())]))
            .unwrap();
    }
    assert_eq!(idx.skip_to("body", "alpha", 50).unwrap(), (0, 0));
}

#[test]
fn test_skip_to_nonexistent_field_or_term() {
    let conn = sqlite_conn();
    let mut idx = make_index(conn, "docs");
    idx.add_document(1, fields(&[("body", "hello".into())]))
        .unwrap();
    assert_eq!(idx.skip_to("body", "alpha", 100).unwrap(), (0, 0));
    assert_eq!(idx.skip_to("body", "nonexistent", 1).unwrap(), (0, 0));
}

#[test]
fn test_blockmax_table_created() {
    let conn = sqlite_conn();
    let mut idx = make_index(conn.clone(), "docs");
    idx.add_document(1, fields(&[("body", "hello world".into())]))
        .unwrap();
    conn.with(|c| {
        let count: i64 = c.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name = '_blockmax_docs_body'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(count, 1);
        Ok(())
    })
    .unwrap();
}

#[test]
fn test_build_block_max_single_block() {
    let conn = sqlite_conn();
    let mut idx = make_index(conn, "docs");
    for i in 1..=10 {
        idx.add_document(i, fields(&[("body", "alpha".into())]))
            .unwrap();
    }
    idx.build_block_max_scores("body", "alpha", &scorer(10, 5.0))
        .unwrap();
    let scores = idx.get_all_block_max_scores("body", "alpha").unwrap();
    assert_eq!(scores.len(), 1);
    assert!(scores[0] > 0.0);
}

#[test]
fn test_build_block_max_multiple_blocks() {
    let conn = sqlite_conn();
    let mut idx = make_index(conn, "docs");
    for i in 1..=300 {
        idx.add_document(i, fields(&[("body", "alpha".into())]))
            .unwrap();
    }
    idx.build_block_max_scores("body", "alpha", &scorer(300, 5.0))
        .unwrap();
    let scores = idx.get_all_block_max_scores("body", "alpha").unwrap();
    assert_eq!(scores.len(), 3);
    assert!(scores.iter().all(|score| *score > 0.0));
}

#[test]
fn test_get_block_max_score_by_index() {
    let conn = sqlite_conn();
    let mut idx = make_index(conn, "docs");
    for i in 1..=300 {
        idx.add_document(i, fields(&[("body", "alpha".into())]))
            .unwrap();
    }
    idx.build_block_max_scores("body", "alpha", &scorer(300, 5.0))
        .unwrap();
    assert!(idx.get_block_max_score("body", "alpha", 0).unwrap() > 0.0);
    assert!(idx.get_block_max_score("body", "alpha", 1).unwrap() > 0.0);
    assert_eq!(idx.get_block_max_score("body", "alpha", 99).unwrap(), 0.0);
}

#[test]
fn test_get_block_max_nonexistent_field() {
    let conn = sqlite_conn();
    let idx = make_index(conn, "docs");
    assert_eq!(
        idx.get_block_max_score("nonexistent", "alpha", 0).unwrap(),
        0.0
    );
    assert!(idx
        .get_all_block_max_scores("nonexistent", "alpha")
        .unwrap()
        .is_empty());
}

#[test]
fn test_build_all_block_max_scores() {
    let conn = sqlite_conn();
    let mut idx = make_index(conn, "docs");
    idx.add_document(1, fields(&[("body", "hello world".into())]))
        .unwrap();
    idx.add_document(2, fields(&[("body", "hello there".into())]))
        .unwrap();
    idx.add_document(3, fields(&[("body", "goodbye world".into())]))
        .unwrap();
    idx.build_all_block_max_scores("body", &scorer(3, 5.0))
        .unwrap();
    assert_eq!(
        idx.get_all_block_max_scores("body", "hello").unwrap().len(),
        1
    );
    assert_eq!(
        idx.get_all_block_max_scores("body", "world").unwrap().len(),
        1
    );
}

#[test]
fn test_build_block_max_for_nonexistent_field() {
    let conn = sqlite_conn();
    let idx = make_index(conn, "docs");
    idx.build_block_max_scores("nonexistent", "alpha", &scorer(10, 5.0))
        .unwrap();
    idx.build_all_block_max_scores("nonexistent", &scorer(10, 5.0))
        .unwrap();
}

#[test]
fn test_skip_pointers_survive_reconnection() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("persist.db");
    {
        let conn = file_conn(&db);
        let mut idx = make_index(conn, "docs");
        for i in 1..200 {
            idx.add_document(i, fields(&[("body", "alpha".into())]))
                .unwrap();
        }
        idx.flush_skip_pointers().unwrap();
    }
    let conn = file_conn(&db);
    let idx = make_index(conn, "docs");
    assert_eq!(idx.skip_to("body", "alpha", 150).unwrap(), (129, 128));
}

#[test]
fn test_block_max_scores_survive_reconnection() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("persist.db");
    {
        let conn = file_conn(&db);
        let mut idx = make_index(conn, "docs");
        for i in 1..=10 {
            idx.add_document(i, fields(&[("body", "alpha".into())]))
                .unwrap();
        }
        idx.build_block_max_scores("body", "alpha", &scorer(10, 5.0))
            .unwrap();
    }
    let conn = file_conn(&db);
    let idx = make_index(conn, "docs");
    let scores = idx.get_all_block_max_scores("body", "alpha").unwrap();
    assert_eq!(scores.len(), 1);
    assert!(scores[0] > 0.0);
}

#[test]
fn test_load_block_max_into_memory_index() {
    let conn = sqlite_conn();
    let mut idx = make_index(conn, "docs");
    for i in 1..=10 {
        idx.add_document(i, fields(&[("body", "alpha".into())]))
            .unwrap();
    }
    idx.build_block_max_scores("body", "alpha", &scorer(10, 5.0))
        .unwrap();
    let mut blockmax = BlockMaxIndex::default();
    idx.load_block_max_into(&mut blockmax).unwrap();
    assert_eq!(blockmax.num_blocks("docs", "body", "alpha"), 1);
    assert!(blockmax.block_max("docs", "body", "alpha", 0) > 0.0);
}

#[test]
fn test_block_max_save_and_load() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    let mut blockmax = BlockMaxIndex::default();
    blockmax
        .set_block_maxes("articles", "body", "hello", vec![1.5, 2.3, 0.9])
        .unwrap();
    blockmax
        .set_block_maxes("articles", "title", "world", vec![3.0])
        .unwrap();
    blockmax.save_to_sqlite(&conn).unwrap();

    let mut loaded = BlockMaxIndex::default();
    loaded.load_from_sqlite(&conn).unwrap();
    assert_eq!(loaded.block_max("articles", "body", "hello", 0), 1.5);
    assert_eq!(loaded.block_max("articles", "body", "hello", 1), 2.3);
    assert_eq!(loaded.block_max("articles", "body", "hello", 2), 0.9);
    assert_eq!(loaded.block_max("articles", "title", "world", 0), 3.0);
    assert_eq!(loaded.num_blocks("articles", "body", "hello"), 3);
    assert_eq!(loaded.num_blocks("articles", "title", "world"), 1);
}

#[test]
fn test_block_max_save_and_load_multi_table_isolation() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    let mut blockmax = BlockMaxIndex::default();
    blockmax
        .set_block_maxes("articles", "body", "hello", vec![1.5])
        .unwrap();
    blockmax
        .set_block_maxes("comments", "body", "hello", vec![9.9])
        .unwrap();
    blockmax.save_to_sqlite(&conn).unwrap();
    let mut loaded = BlockMaxIndex::default();
    loaded.load_from_sqlite(&conn).unwrap();
    assert_eq!(loaded.block_max("articles", "body", "hello", 0), 1.5);
    assert_eq!(loaded.block_max("comments", "body", "hello", 0), 9.9);
}

#[test]
fn test_block_max_save_overwrites_previous() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    let mut first = BlockMaxIndex::default();
    first
        .set_block_maxes("", "body", "hello", vec![1.0])
        .unwrap();
    first.save_to_sqlite(&conn).unwrap();
    let mut second = BlockMaxIndex::default();
    second
        .set_block_maxes("", "body", "hello", vec![9.9])
        .unwrap();
    second.save_to_sqlite(&conn).unwrap();
    let mut loaded = BlockMaxIndex::default();
    loaded.load_from_sqlite(&conn).unwrap();
    assert_eq!(loaded.block_max("", "body", "hello", 0), 9.9);
    assert_eq!(loaded.num_blocks("", "body", "hello"), 1);
}

#[test]
fn test_load_from_empty_database() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    let mut blockmax = BlockMaxIndex::default();
    blockmax.load_from_sqlite(&conn).unwrap();
    assert_eq!(blockmax.num_blocks("", "body", "hello"), 0);
}

#[test]
fn test_load_legacy_schema_migration() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute(
        "CREATE TABLE _global_blockmax (
            field TEXT NOT NULL,
            term TEXT NOT NULL,
            block_idx INTEGER NOT NULL,
            max_score REAL NOT NULL,
            PRIMARY KEY (field, term, block_idx)
        )",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO _global_blockmax VALUES ('body', 'hello', 0, 2.5)",
        [],
    )
    .unwrap();
    let mut loaded = BlockMaxIndex::default();
    loaded.load_from_sqlite(&conn).unwrap();
    assert_eq!(loaded.block_max("", "body", "hello", 0), 2.5);
    assert_eq!(loaded.num_blocks("", "body", "hello"), 1);
}

#[test]
fn test_default_block_size() {
    assert_eq!(SQLiteInvertedIndex::BLOCK_SIZE, 128);
    assert_eq!(BlockMaxIndex::default().block_size(), 128);
}

#[test]
fn block_max_empty_posting_list_records_no_scores() {
    let mut blockmax = BlockMaxIndex::default();
    blockmax
        .build(
            &PostingList::new(),
            &scorer(10, 5.0),
            "body",
            "missing",
            "docs",
        )
        .unwrap();
    assert_eq!(blockmax.num_blocks("docs", "body", "missing"), 0);
}
